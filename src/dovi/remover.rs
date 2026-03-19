use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;

use anyhow::Result;

use crate::commands::RemoveArgs;

use super::av1_parser::{
    Obu, is_dovi_rpu_obu, read_ivf_frame_header, read_obus_from_ivf_frame,
    try_read_ivf_file_header, write_ivf_frame_header,
};
use super::input_from_either;

pub struct Remover {
    input: PathBuf,
    output: PathBuf,
}

impl Remover {
    pub fn remove(args: RemoveArgs, _options: super::CliOptions) -> Result<()> {
        let RemoveArgs {
            input,
            input_pos,
            output,
        } = args;

        let input = input_from_either("remove", input, input_pos)?;
        let output = output.unwrap_or_else(|| PathBuf::from("BL.av1"));

        let pb = super::initialize_progress_bar(&input)?;

        let remover = Remover { input, output };
        let res = remover.process_input();

        pb.finish_and_clear();
        res
    }

    fn process_input(&self) -> Result<()> {
        let file = File::open(&self.input)?;
        let mut reader = BufReader::with_capacity(100_000, file);

        let out_file = File::create(&self.output).expect("Can't create output file");
        let mut writer = BufWriter::with_capacity(100_000, out_file);

        if let Some(ivf_header) = try_read_ivf_file_header(&mut reader)? {
            // IVF: pass file header through, then remove DoVi OBUs per frame
            writer.write_all(&ivf_header)?;

            loop {
                let fh = match read_ivf_frame_header(&mut reader)? {
                    Some(h) => h,
                    None => break,
                };

                let mut frame_data = vec![0u8; fh.frame_size as usize];
                reader.read_exact(&mut frame_data)?;

                let obus = read_obus_from_ivf_frame(frame_data)?;

                // Collect output OBUs (skip Dolby Vision RPU)
                let output_frame: Vec<u8> = obus
                    .iter()
                    .filter(|o| !is_dovi_rpu_obu(o))
                    .flat_map(|o| o.raw_bytes.iter().copied())
                    .collect();

                write_ivf_frame_header(&mut writer, output_frame.len() as u32, fh.timestamp)?;
                writer.write_all(&output_frame)?;
            }
        } else {
            // Raw OBU stream: skip Dolby Vision RPU OBUs, copy everything else
            loop {
                match Obu::read_from(&mut reader) {
                    Ok(Some(obu)) => {
                        if !is_dovi_rpu_obu(&obu) {
                            writer.write_all(&obu.raw_bytes)?;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => return Err(e),
                }
            }
        }

        writer.flush()?;
        Ok(())
    }
}
