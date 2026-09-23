use anyhow::Result;
use indicatif::ProgressBar;
use std::fs::File;
use std::io::{BufRead, BufWriter};
use std::path::{Path, PathBuf};

use crate::commands::ConvertArgs;

use super::av1::{
    BitstreamCodec, IvfWriter, MatroskaAv1Reader, Obu, ObuWriter, build_dovi_obu, detect_codec,
    extract_dovi_t35_payload, is_dovi_rpu_obu, is_stdin, matroska_video_codec, open_input,
    read_ivf_frame_header, read_obus_from_ivf_frame, try_read_ivf_file_header,
};
use super::{CliOptions, IoFormat, general_read_write, input_from_either};
use dolby_vision::rpu::dovi_rpu::DoviRpu;

use general_read_write::{DoviProcessor, DoviWriter};

/// Apply the conversion options to a Dolby Vision RPU OBU, passing anything
/// else through untouched.
fn convert_obu(options: &CliOptions, obu: &Obu) -> Result<Vec<u8>> {
    if is_dovi_rpu_obu(obu) {
        if let Some(t35_payload) = extract_dovi_t35_payload(&obu.payload) {
            let mut dovi_rpu = DoviRpu::parse_itu_t35_dovi_metadata_obu(t35_payload)?;
            super::convert_encoded_from_opts_rpu(options, &mut dovi_rpu)?;

            return build_dovi_obu(&dovi_rpu);
        }
    }

    Ok(obu.raw_bytes.clone())
}

fn is_av1_input(path: &Path) -> bool {
    !is_stdin(path) && detect_codec(path) == BitstreamCodec::Av1
}

pub struct Converter {
    format: IoFormat,
    input: PathBuf,
    output: PathBuf,
}

impl Converter {
    pub fn from_args(args: ConvertArgs, options: &mut CliOptions) -> Result<Self> {
        let ConvertArgs {
            input,
            input_pos,
            output,
            discard,
        } = args;

        options.discard_el = discard;

        let input = input_from_either("convert", input, input_pos)?;

        let (format, default_output) = if is_av1_input(&input) {
            let ext = input.extension().and_then(|e| e.to_str()).unwrap_or("av1");
            (IoFormat::Raw, PathBuf::from(format!("converted.{ext}")))
        } else {
            let format = hevc_parser::io::format_from_path(&input)?;

            // Matroska is unwrapped into a raw stream, so the result of an AV1
            // track is raw AV1 rather than a copy of the container extension
            let default = if format == IoFormat::Matroska
                && matroska_video_codec(&input) == Some(BitstreamCodec::Av1)
            {
                PathBuf::from("converted.av1")
            } else {
                match options.discard_el {
                    true => PathBuf::from("BL_RPU.hevc"),
                    false => PathBuf::from("BL_EL_RPU.hevc"),
                }
            };

            (format, default)
        };

        let output = output.unwrap_or(default_output);

        Ok(Self {
            format,
            input,
            output,
        })
    }

    pub fn convert(args: ConvertArgs, mut options: CliOptions) -> Result<()> {
        let converter = Converter::from_args(args, &mut options)?;
        converter.process_input(options)
    }

    fn process_input(&self, options: CliOptions) -> Result<()> {
        // Matroska needs the container reader rather than an elementary stream
        if self.format == IoFormat::Matroska
            && matroska_video_codec(&self.input) == Some(BitstreamCodec::Av1)
        {
            return self.convert_av1_matroska(&options);
        }

        let (codec, mut reader) = open_input(&self.input)?;

        if let BitstreamCodec::Av1 = codec {
            return self.convert_av1(&options, &mut reader);
        }

        let pb = super::initialize_progress_bar(&self.format, &self.input)?;

        if self.format == IoFormat::Matroska {
            println!("Converter: Matroska input is experimental!");
        }

        self.convert_hevc(pb, options, reader)
    }

    fn convert_av1(&self, options: &CliOptions, reader: &mut dyn BufRead) -> Result<()> {
        println!("Converting DoVi RPU in AV1 bitstream...");

        // Sized handle over the trait object, so the generic readers accept it
        let mut reader = reader;

        if let Some(ivf_header) = try_read_ivf_file_header(&mut reader)? {
            let out_file = BufWriter::new(File::create(&self.output).expect("Can't create file"));
            let mut ivf_writer = IvfWriter::new(out_file, &ivf_header)?;

            while let Some(frame_hdr) = read_ivf_frame_header(&mut reader)? {
                let mut frame_data = vec![0u8; frame_hdr.frame_size as usize];
                reader.read_exact(&mut frame_data)?;

                let obus = read_obus_from_ivf_frame(frame_data)?;
                let mut new_frame: Vec<u8> = Vec::new();

                for obu in &obus {
                    new_frame.extend_from_slice(&convert_obu(options, obu)?);
                }

                ivf_writer.write_frame(frame_hdr.timestamp, &new_frame)?;
            }

            ivf_writer.flush()?;
        } else {
            let out_file = BufWriter::new(File::create(&self.output).expect("Can't create file"));
            let mut obu_writer = ObuWriter::new(out_file);

            while let Some(obu) = Obu::read_from(&mut reader)? {
                obu_writer.write_raw(&convert_obu(options, &obu)?)?;
            }

            obu_writer.flush()?;
        }

        println!("Done.");
        Ok(())
    }

    /// Convert every temporal unit of a Matroska AV1 track into a raw AV1
    /// stream, the same shape the HEVC path produces for Matroska input.
    fn convert_av1_matroska(&self, options: &CliOptions) -> Result<()> {
        println!("Converting DoVi RPU in AV1 bitstream...");

        let mut mkv = MatroskaAv1Reader::open(&self.input)?;
        let out_file = BufWriter::new(File::create(&self.output).expect("Can't create file"));
        let mut obu_writer = ObuWriter::new(out_file);

        while let Some(obus) = mkv.next_temporal_unit()? {
            for obu in &obus {
                obu_writer.write_raw(&convert_obu(options, obu)?)?;
            }
        }

        obu_writer.flush()?;

        println!("Done.");
        Ok(())
    }

    fn convert_hevc(
        &self,
        pb: ProgressBar,
        options: CliOptions,
        mut reader: Box<dyn BufRead>,
    ) -> Result<()> {
        let dovi_writer = DoviWriter::new(None, None, None, Some(&self.output));
        let mut dovi_processor = DoviProcessor::new(
            options,
            self.input.clone(),
            dovi_writer,
            pb,
            Default::default(),
        );

        match self.format {
            // The container processor needs to seek, so it reopens the file
            IoFormat::Matroska => dovi_processor.read_write_from_io(&self.format),
            _ => dovi_processor.read_write_from_reader(&self.format, &mut reader),
        }
    }
}
