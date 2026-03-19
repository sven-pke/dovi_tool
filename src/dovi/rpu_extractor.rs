use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;

use anyhow::{Result, bail};
use indicatif::ProgressBar;

use dolby_vision::rpu::dovi_rpu::DoviRpu;

use crate::commands::ExtractRpuArgs;

use super::av1_parser::{
    Obu, extract_dovi_t35_payload, read_ivf_frame_header, read_obus_from_ivf_frame,
    try_read_ivf_file_header,
};
use super::input_from_either;

pub struct RpuExtractor {
    input: PathBuf,
    rpu_out: PathBuf,
    limit: Option<u64>,
}

impl RpuExtractor {
    pub fn from_args(args: ExtractRpuArgs) -> Result<Self> {
        let ExtractRpuArgs {
            input,
            input_pos,
            rpu_out,
            limit,
        } = args;

        let input = input_from_either("extract-rpu", input, input_pos)?;

        let rpu_out = rpu_out.unwrap_or_else(|| PathBuf::from("RPU.bin"));

        Ok(Self {
            input,
            rpu_out,
            limit,
        })
    }

    pub fn extract_rpu(args: ExtractRpuArgs, _options: super::CliOptions) -> Result<()> {
        let extractor = RpuExtractor::from_args(args)?;
        let pb = super::initialize_progress_bar(&extractor.input)?;
        extractor.process_input(pb)
    }

    fn process_input(&self, pb: ProgressBar) -> Result<()> {
        let file = File::open(&self.input)?;
        let mut reader = BufReader::with_capacity(100_000, file);

        let mut rpus: Vec<DoviRpu> = Vec::new();
        let mut obu_count = 0u64;

        if let Some(ivf_header) = try_read_ivf_file_header(&mut reader)? {
            // IVF: one temporal unit per frame
            let _ = ivf_header;
            loop {
                let fh = match read_ivf_frame_header(&mut reader)? {
                    Some(h) => h,
                    None => break,
                };
                let mut frame_data = vec![0u8; fh.frame_size as usize];
                reader.read_exact(&mut frame_data)?;

                let obus = read_obus_from_ivf_frame(frame_data)?;
                for obu in &obus {
                    if let Some(t35) = extract_dovi_t35_payload(&obu.payload) {
                        let rpu = DoviRpu::parse_itu_t35_dovi_metadata_obu(t35)?;
                        rpus.push(rpu);
                    }

                    obu_count += 1;
                    if let Some(lim) = self.limit {
                        if obu_count >= lim {
                            break;
                        }
                    }
                }

                pb.inc(fh.frame_size as u64 / 100_000_000 + 1);

                if self.limit.map(|l| obu_count >= l).unwrap_or(false) {
                    break;
                }
            }
        } else {
            // Raw OBU stream
            loop {
                match Obu::read_from(&mut reader) {
                    Ok(Some(obu)) => {
                        pb.inc(obu.raw_bytes.len() as u64 / 100_000_000 + 1);

                        if let Some(t35) = extract_dovi_t35_payload(&obu.payload) {
                            let rpu = DoviRpu::parse_itu_t35_dovi_metadata_obu(t35)?;
                            rpus.push(rpu);
                        }

                        obu_count += 1;
                        if let Some(lim) = self.limit {
                            if obu_count >= lim {
                                break;
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(e) => return Err(e),
                }
            }
        }

        pb.finish_and_clear();

        if rpus.is_empty() {
            bail!("No Dolby Vision RPU data found in input");
        }

        println!("Found {} RPU(s). Writing RPU file...", rpus.len());
        self.write_rpu_file(&rpus)?;

        Ok(())
    }

    fn write_rpu_file(&self, rpus: &[DoviRpu]) -> Result<()> {
        let mut writer = BufWriter::with_capacity(
            100_000,
            File::create(&self.rpu_out).expect("Can't create RPU output file"),
        );

        for rpu in rpus {
            // write_hevc_unspec62_nalu returns [7C 01 <RPU bytes starting with 0x19>]
            // parse_rpu_file expects [00 00 00 01 19 ...] — skip the 2-byte 7C 01 header
            let encoded = rpu.write_hevc_unspec62_nalu()?;
            writer.write_all(&[0x00, 0x00, 0x00, 0x01])?;
            writer.write_all(&encoded[2..])?;
        }

        writer.flush()?;
        Ok(())
    }
}
