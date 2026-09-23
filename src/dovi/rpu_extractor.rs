use anyhow::Result;
use indicatif::ProgressBar;
use std::fs::File;
use std::io::{BufRead, BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::{commands::ExtractRpuArgs, dovi::general_read_write::DoviProcessorError};

use super::{
    CliOptions, IoFormat,
    general_read_write::{self, DoviProcessorOptions},
    input_from_either,
};
use general_read_write::{DoviProcessor, DoviWriter};

use super::av1::{
    BitstreamCodec, MatroskaAv1Reader, OBU_TEMPORAL_DELIMITER, Obu, detect_codec,
    extract_dovi_t35_payload, is_dovi_rpu_obu, is_stdin, matroska_video_codec, open_input,
    read_ivf_frame_header, read_obus_from_ivf_frame, try_read_ivf_file_header,
};

/// Collect the Dolby Vision RPUs carried by one temporal unit, encoded the way
/// the RPU file wants them.
fn collect_rpus(obus: &[Obu], rpus: &mut Vec<Vec<u8>>) -> Result<()> {
    for obu in obus {
        if let Some(t35_payload) =
            extract_dovi_t35_payload(&obu.payload).filter(|_| is_dovi_rpu_obu(obu))
        {
            let rpu = DoviRpu::parse_itu_t35_dovi_metadata_obu(t35_payload)?;
            rpus.push(rpu.write_hevc_unspec62_nalu()?);
        }
    }

    Ok(())
}
use dolby_vision::rpu::dovi_rpu::DoviRpu;
use hevc_parser::hevc::{NAL_UNSPEC62, NALUnit};
use hevc_parser::io::StartCodePreset;

pub struct RpuExtractor {
    format: IoFormat,
    input: PathBuf,
    rpu_out: PathBuf,
    limit: Option<u64>,
    track_number: Option<usize>,
}

fn is_av1_input(path: &Path) -> bool {
    !is_stdin(path) && detect_codec(path) == BitstreamCodec::Av1
}

impl RpuExtractor {
    pub fn from_args(args: ExtractRpuArgs) -> Result<Self> {
        let ExtractRpuArgs {
            input,
            input_pos,
            rpu_out,
            limit,
            track_number,
        } = args;

        let input = input_from_either("extract-rpu", input, input_pos)?;

        // For AV1 inputs use a dummy format; for HEVC use the existing detection
        let format = if is_av1_input(&input) {
            IoFormat::Raw
        } else {
            hevc_parser::io::format_from_path(&input)?
        };

        let rpu_out = match rpu_out {
            Some(path) => path,
            None => PathBuf::from("RPU.bin"),
        };

        Ok(Self {
            format,
            input,
            rpu_out,
            limit,
            track_number,
        })
    }

    pub fn extract_rpu(args: ExtractRpuArgs, options: CliOptions) -> Result<()> {
        let rpu_extractor = RpuExtractor::from_args(args)?;
        rpu_extractor.process_input(options)
    }

    fn process_input(&self, options: CliOptions) -> Result<()> {
        // Matroska needs the container reader rather than an elementary stream
        if self.format == IoFormat::Matroska
            && matroska_video_codec(&self.input) == Some(BitstreamCodec::Av1)
        {
            return self.extract_rpu_from_av1_matroska();
        }

        let (codec, mut reader) = open_input(&self.input)?;

        match codec {
            BitstreamCodec::Av1 => self.extract_rpu_from_av1(&mut reader),
            BitstreamCodec::Hevc => {
                let pb = super::initialize_progress_bar(&self.format, &self.input)?;
                self.extract_rpu_from_el(pb, options, reader)
            }
        }
    }

    fn extract_rpu_from_av1(&self, reader: &mut dyn BufRead) -> Result<()> {
        println!("Extracting RPU from AV1 bitstream...");

        // Sized handle over the trait object, so the generic readers accept it
        let mut reader = reader;

        let mut rpus: Vec<Vec<u8>> = Vec::new();
        let mut tu_count: u64 = 0;

        // Detect IVF container by peeking at first bytes
        if try_read_ivf_file_header(&mut reader)?.is_some() {
            // IVF container: one temporal unit per IVF frame
            while let Some(frame_hdr) = read_ivf_frame_header(&mut reader)? {
                if self.limit.is_some_and(|limit| tu_count >= limit) {
                    break;
                }

                let mut frame_data = vec![0u8; frame_hdr.frame_size as usize];
                reader.read_exact(&mut frame_data)?;

                collect_rpus(&read_obus_from_ivf_frame(frame_data)?, &mut rpus)?;

                tu_count += 1;
            }
        } else {
            // Raw AV1 bitstream: temporal units are delimited by OBU_TEMPORAL_DELIMITER
            let mut tu: Vec<Obu> = Vec::new();

            loop {
                let obu = Obu::read_from(&mut reader)?;
                let starts_new_tu = obu
                    .as_ref()
                    .is_some_and(|o| o.obu_type == OBU_TEMPORAL_DELIMITER);

                if (obu.is_none() || starts_new_tu) && !tu.is_empty() {
                    collect_rpus(&tu, &mut rpus)?;
                    tu.clear();

                    tu_count += 1;

                    if self.limit.is_some_and(|limit| tu_count >= limit) {
                        break;
                    }
                }

                match obu {
                    None => break,
                    Some(obu) => tu.push(obu),
                }
            }
        }

        println!("Found {} RPU(s).", rpus.len());
        self.write_av1_rpu_file(&rpus)
    }

    fn extract_rpu_from_av1_matroska(&self) -> Result<()> {
        println!("Extracting RPU from AV1 bitstream...");

        let mut mkv = MatroskaAv1Reader::open(&self.input)?;

        let mut rpus: Vec<Vec<u8>> = Vec::new();
        let mut tu_count: u64 = 0;

        while let Some(obus) = mkv.next_temporal_unit()? {
            if self.limit.is_some_and(|limit| tu_count >= limit) {
                break;
            }

            collect_rpus(&obus, &mut rpus)?;

            tu_count += 1;
        }

        println!("Found {} RPU(s).", rpus.len());
        self.write_av1_rpu_file(&rpus)
    }

    fn write_av1_rpu_file(&self, rpus: &[Vec<u8>]) -> Result<()> {
        // An empty RPU file with a success exit would read as "extracted";
        // the HEVC path refuses in this case, and so does this one.
        if rpus.is_empty() {
            return Err(DoviProcessorError::NoRpuFound.into());
        }

        println!("Writing RPU file...");
        let mut writer = BufWriter::with_capacity(
            100_000,
            File::create(&self.rpu_out).expect("Can't create file"),
        );

        for encoded_rpu in rpus {
            // encoded_rpu is write_hevc_unspec62_nalu() output: starts with 0x7C 0x01
            // Same format as HEVC path: [00 00 00 01] + rpu[2..]
            NALUnit::write_with_preset(
                &mut writer,
                &encoded_rpu[2..],
                StartCodePreset::Four,
                NAL_UNSPEC62,
                true,
            )?;
        }

        writer.flush()?;
        Ok(())
    }

    fn extract_rpu_from_el(
        &self,
        pb: ProgressBar,
        options: CliOptions,
        mut reader: Box<dyn BufRead>,
    ) -> Result<()> {
        let rpu_out = self.rpu_out.as_path();

        let dovi_writer = DoviWriter::new(None, None, Some(rpu_out), None);
        let mut dovi_processor = DoviProcessor::new(
            options,
            self.input.clone(),
            dovi_writer,
            pb,
            DoviProcessorOptions {
                limit: self.limit,
                track_number: self.track_number,
            },
        );

        let res = match self.format {
            // The container processor needs to seek, so it reopens the file
            IoFormat::Matroska => dovi_processor.read_write_from_io(&self.format),
            _ => dovi_processor.read_write_from_reader(&self.format, &mut reader),
        };

        if res.as_ref().is_err_and(|err| {
            err.downcast_ref::<DoviProcessorError>()
                .is_some_and(|e| matches!(e, DoviProcessorError::NoRpuFound))
        }) {
            std::fs::remove_file(rpu_out)?;
        }

        res
    }
}
