use anyhow::Result;
use indicatif::ProgressBar;
use std::fs::File;
use std::io::{BufRead, BufWriter};
use std::path::{Path, PathBuf};

use crate::commands::RemoveArgs;

use super::av1::{
    BitstreamCodec, IvfWriter, MatroskaAv1Reader, Obu, ObuWriter, detect_codec, is_dovi_rpu_obu,
    is_stdin, matroska_video_codec, open_input, read_ivf_frame_header, read_obus_from_ivf_frame,
    try_read_ivf_file_header,
};
use super::{CliOptions, IoFormat, general_read_write, input_from_either};

use general_read_write::{DoviProcessor, DoviWriter};

fn is_av1_input(path: &Path) -> bool {
    !is_stdin(path) && detect_codec(path) == BitstreamCodec::Av1
}

pub struct Remover {
    format: IoFormat,
    input: PathBuf,
    output: PathBuf,
}

impl Remover {
    pub fn from_args(args: RemoveArgs) -> Result<Self> {
        let RemoveArgs {
            input,
            input_pos,
            output,
        } = args;

        let input = input_from_either("remove", input, input_pos)?;

        let (format, default_output) = if is_av1_input(&input) {
            let ext = input.extension().and_then(|e| e.to_str()).unwrap_or("av1");
            (IoFormat::Raw, PathBuf::from(format!("BL_no_dovi.{ext}")))
        } else {
            let format = hevc_parser::io::format_from_path(&input)?;

            // Matroska is unwrapped into a raw stream, so the result of an AV1
            // track is raw AV1 rather than a copy of the container extension
            let default = if format == IoFormat::Matroska
                && matroska_video_codec(&input) == Some(BitstreamCodec::Av1)
            {
                PathBuf::from("BL_no_dovi.av1")
            } else {
                PathBuf::from("BL.hevc")
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

    pub fn remove(args: RemoveArgs, options: CliOptions) -> Result<()> {
        let remover = Remover::from_args(args)?;
        remover.process_input(options)
    }

    fn process_input(&self, options: CliOptions) -> Result<()> {
        // Matroska needs the container reader rather than an elementary stream
        if self.format == IoFormat::Matroska
            && matroska_video_codec(&self.input) == Some(BitstreamCodec::Av1)
        {
            return self.remove_from_av1_matroska();
        }

        let (codec, mut reader) = open_input(&self.input)?;

        if let BitstreamCodec::Av1 = codec {
            return self.remove_from_av1(&mut reader);
        }

        let pb = super::initialize_progress_bar(&self.format, &self.input)?;

        if self.format == IoFormat::Matroska {
            println!("Remover: Matroska input is experimental!");
        }

        self.remove_from_hevc(pb, options, reader)
    }

    fn remove_from_av1(&self, reader: &mut dyn BufRead) -> Result<()> {
        println!("Removing DoVi RPU from AV1 bitstream...");

        // Sized handle over the trait object, so the generic readers accept it
        let mut reader = reader;

        if let Some(ivf_header) = try_read_ivf_file_header(&mut reader)? {
            // IVF container
            let out_file = BufWriter::new(File::create(&self.output).expect("Can't create file"));
            let mut ivf_writer = IvfWriter::new(out_file, &ivf_header)?;

            while let Some(frame_hdr) = read_ivf_frame_header(&mut reader)? {
                let mut frame_data = vec![0u8; frame_hdr.frame_size as usize];
                reader.read_exact(&mut frame_data)?;

                let obus = read_obus_from_ivf_frame(frame_data)?;
                let mut new_frame: Vec<u8> = Vec::new();

                for obu in &obus {
                    if !is_dovi_rpu_obu(obu) {
                        new_frame.extend_from_slice(&obu.raw_bytes);
                    }
                }

                ivf_writer.write_frame(frame_hdr.timestamp, &new_frame)?;
            }

            ivf_writer.flush()?;
        } else {
            // Raw AV1 bitstream
            let out_file = BufWriter::new(File::create(&self.output).expect("Can't create file"));
            let mut obu_writer = ObuWriter::new(out_file);

            while let Some(obu) = Obu::read_from(&mut reader)? {
                if !is_dovi_rpu_obu(&obu) {
                    obu_writer.write_raw(&obu.raw_bytes)?;
                }
            }

            obu_writer.flush()?;
        }

        println!("Done.");
        Ok(())
    }

    /// Strip the Dolby Vision RPUs out of a Matroska AV1 track and write the
    /// result as a raw AV1 stream.
    fn remove_from_av1_matroska(&self) -> Result<()> {
        println!("Removing DoVi RPU from AV1 bitstream...");

        let mut mkv = MatroskaAv1Reader::open(&self.input)?;
        let out_file = BufWriter::new(File::create(&self.output).expect("Can't create file"));
        let mut obu_writer = ObuWriter::new(out_file);

        while let Some(obus) = mkv.next_temporal_unit()? {
            for obu in obus.iter().filter(|o| !is_dovi_rpu_obu(o)) {
                obu_writer.write_raw(&obu.raw_bytes)?;
            }
        }

        obu_writer.flush()?;

        println!("Done.");
        Ok(())
    }

    fn remove_from_hevc(
        &self,
        pb: ProgressBar,
        options: CliOptions,
        mut reader: Box<dyn BufRead>,
    ) -> Result<()> {
        let bl_out = Some(self.output.as_path());

        let dovi_writer = DoviWriter::new(bl_out, None, None, None);
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
