use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write, stdout};
use std::path::PathBuf;

use anyhow::{Result, bail};
use indicatif::ProgressBar;

use dolby_vision::rpu::utils::parse_rpu_file;

use crate::commands::InjectRpuArgs;

use super::av1_parser::{
    IvfFrameHeader, Obu, OBU_TEMPORAL_DELIMITER, build_dovi_obu, is_dovi_rpu_obu,
    read_ivf_frame_header, read_obus_from_ivf_frame, try_read_ivf_file_header,
    write_ivf_frame_header,
};
use super::input_from_either;

pub struct RpuInjector {
    input: PathBuf,
    output: PathBuf,
    progress_bar: ProgressBar,
}

impl RpuInjector {
    pub fn inject_rpu(args: InjectRpuArgs, _cli_options: super::CliOptions) -> Result<()> {
        let InjectRpuArgs {
            input,
            input_pos,
            rpu_in,
            output,
        } = args;

        let input = input_from_either("inject-rpu", input, input_pos)?;
        let output = output.unwrap_or_else(|| PathBuf::from("injected_output.av1"));

        let pb = super::initialize_progress_bar(&input)?;

        println!("Parsing RPU file...");
        stdout().flush().ok();

        let rpus = parse_rpu_file(&rpu_in)?;
        println!("Loaded {} RPU(s).", rpus.len());

        let _ = rpu_in; // path already used above to parse the file
        RpuInjector {
            progress_bar: pb,
            output,
            input,
        }
        .run(rpus)
    }

    fn run(self, rpus: Vec<dolby_vision::rpu::dovi_rpu::DoviRpu>) -> Result<()> {
        let file = File::open(&self.input)?;
        let mut reader = BufReader::with_capacity(100_000, file);

        let out_file = File::create(&self.output).expect("Can't create output file");
        let mut writer = BufWriter::with_capacity(100_000, out_file);

        if let Some(ivf_header) = try_read_ivf_file_header(&mut reader)? {
            writer.write_all(&ivf_header)?;
            self.inject_ivf(&mut reader, &mut writer, &rpus)?;
        } else {
            self.inject_raw(&mut reader, &mut writer, &rpus)?;
        }

        self.progress_bar.finish_and_clear();
        println!("Rewriting with interleaved RPU OBUs: Done.");
        writer.flush()?;
        Ok(())
    }

    fn inject_ivf<R, W>(
        &self,
        reader: &mut R,
        writer: &mut W,
        rpus: &[dolby_vision::rpu::dovi_rpu::DoviRpu],
    ) -> Result<()>
    where
        R: Read,
        W: Write,
    {
        let total_rpus = rpus.len();
        let mut tu_index = 0usize;
        let mut warned_existing = false;
        let mut warned_mismatch = false;

        loop {
            let fh: IvfFrameHeader = match read_ivf_frame_header(reader)? {
                Some(h) => h,
                None => break,
            };

            let mut frame_data = vec![0u8; fh.frame_size as usize];
            reader.read_exact(&mut frame_data)?;

            let obus = read_obus_from_ivf_frame(frame_data)?;

            // Warn about existing RPU on first occurrence
            if !warned_existing && obus.iter().any(|o| is_dovi_rpu_obu(o)) {
                warned_existing = true;
                println!(
                    "\nWarning: Input file already has Dolby Vision RPU OBUs; \
                     they will be replaced."
                );
            }

            let encoded = if tu_index < total_rpus {
                build_dovi_obu(&rpus[tu_index])?
            } else {
                if !warned_mismatch {
                    warned_mismatch = true;
                    println!(
                        "\nWarning: mismatched lengths. \
                         RPU has {total_rpus} entries but video has more frames. \
                         Last RPU will be duplicated."
                    );
                }
                match rpus.last() {
                    Some(rpu) => build_dovi_obu(rpu)?,
                    None => bail!("No RPU available for TU {tu_index}"),
                }
            };

            let output_frame = Self::build_output_frame(&obus, &encoded);
            write_ivf_frame_header(writer, output_frame.len() as u32, fh.timestamp)?;
            writer.write_all(&output_frame)?;

            tu_index += 1;
        }

        if tu_index < total_rpus {
            println!(
                "\nWarning: mismatched lengths. RPU has {total_rpus} entries \
                 but video has {tu_index} frames. Excess RPU data was ignored."
            );
        }

        Ok(())
    }

    fn inject_raw<R, W>(
        &self,
        reader: &mut R,
        writer: &mut W,
        rpus: &[dolby_vision::rpu::dovi_rpu::DoviRpu],
    ) -> Result<()>
    where
        R: Read,
        W: Write,
    {
        let total_rpus = rpus.len();
        let mut tu_index = 0usize;
        let mut warned_existing = false;
        let mut warned_mismatch = false;

        let mut current_td: Option<Obu> = None;
        let mut pending: Vec<Obu> = Vec::new();

        loop {
            let obu_opt = Obu::read_from(reader)?;
            let is_eof = obu_opt.is_none();
            let is_td = obu_opt
                .as_ref()
                .map(|o| o.obu_type == OBU_TEMPORAL_DELIMITER)
                .unwrap_or(false);

            if (is_eof || is_td) && current_td.is_some() {
                // Check for existing RPU
                if !warned_existing && pending.iter().any(|o| is_dovi_rpu_obu(o)) {
                    warned_existing = true;
                    println!(
                        "\nWarning: Input file already has Dolby Vision RPU OBUs; \
                         they will be replaced."
                    );
                }

                let encoded = if tu_index < total_rpus {
                    build_dovi_obu(&rpus[tu_index])?
                } else {
                    if !warned_mismatch {
                        warned_mismatch = true;
                        println!(
                            "\nWarning: mismatched lengths. \
                             RPU has {total_rpus} entries but video has more frames. \
                             Last RPU will be duplicated."
                        );
                    }
                    match rpus.last() {
                        Some(rpu) => build_dovi_obu(rpu)?,
                        None => bail!("No RPU available for TU {tu_index}"),
                    }
                };

                // Write: TD + RPU OBU + remaining OBUs (skip existing DoVi)
                let td = current_td.take().unwrap();
                writer.write_all(&td.raw_bytes)?;
                writer.write_all(&encoded)?;
                for obu in pending.drain(..) {
                    if !is_dovi_rpu_obu(&obu) {
                        writer.write_all(&obu.raw_bytes)?;
                    }
                }

                tu_index += 1;
            }

            match obu_opt {
                None => break,
                Some(obu) => {
                    if obu.obu_type == OBU_TEMPORAL_DELIMITER {
                        current_td = Some(obu);
                        pending.clear();
                    } else if current_td.is_some() {
                        pending.push(obu);
                    } else {
                        // OBUs before the first TD — pass through unchanged
                        writer.write_all(&obu.raw_bytes)?;
                    }
                }
            }
        }

        if tu_index < total_rpus {
            println!(
                "\nWarning: mismatched lengths. RPU has {total_rpus} entries \
                 but video has {tu_index} frames. Excess RPU data was ignored."
            );
        }

        Ok(())
    }

    /// Build the output byte buffer for one temporal unit's OBUs:
    /// inject `encoded` right after the OBU_TEMPORAL_DELIMITER (if present)
    /// and strip any existing Dolby Vision RPU OBUs.
    fn build_output_frame(obus: &[Obu], encoded: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut injected = false;

        // Insertion point: right after OBU_TEMPORAL_DELIMITER, or at position 0
        let insert_after_td = obus
            .iter()
            .position(|o| o.obu_type == OBU_TEMPORAL_DELIMITER)
            .map(|i| i + 1)
            .unwrap_or(0);

        for (i, obu) in obus.iter().enumerate() {
            if !injected && i == insert_after_td {
                out.extend_from_slice(encoded);
                injected = true;
            }
            if is_dovi_rpu_obu(obu) {
                continue; // drop existing Dolby Vision RPU
            }
            out.extend_from_slice(&obu.raw_bytes);
        }

        if !injected {
            out.extend_from_slice(encoded);
        }

        out
    }
}
