use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;

use anyhow::Result;

use dolby_vision::rpu::dovi_rpu::DoviRpu;

use crate::commands::ConvertArgs;

use super::av1_parser::{
    IvfFrameHeader, Obu, OBU_TEMPORAL_DELIMITER, build_dovi_obu, extract_dovi_t35_payload,
    is_dovi_rpu_obu, read_ivf_frame_header, read_obus_from_ivf_frame, try_read_ivf_file_header,
    write_ivf_frame_header,
};
use super::input_from_either;
use super::{CliOptions, convert_rpu_with_opts};

pub struct Converter {
    input: PathBuf,
    output: PathBuf,
}

impl Converter {
    pub fn convert(args: ConvertArgs, options: CliOptions) -> Result<()> {
        let ConvertArgs {
            input,
            input_pos,
            output,
        } = args;

        let input = input_from_either("convert", input, input_pos)?;
        let output = output.unwrap_or_else(|| PathBuf::from("converted_output.av1"));

        let pb = super::initialize_progress_bar(&input)?;

        let converter = Converter { input, output };
        let res = converter.process_input(options);

        pb.finish_and_clear();
        res
    }

    fn process_input(&self, options: CliOptions) -> Result<()> {
        let file = File::open(&self.input)?;
        let mut reader = BufReader::with_capacity(100_000, file);

        let out_file = File::create(&self.output).expect("Can't create output file");
        let mut writer = BufWriter::with_capacity(100_000, out_file);

        if let Some(ivf_header) = try_read_ivf_file_header(&mut reader)? {
            writer.write_all(&ivf_header)?;
            self.convert_ivf(&mut reader, &mut writer, &options)?;
        } else {
            self.convert_raw(&mut reader, &mut writer, &options)?;
        }

        writer.flush()?;
        Ok(())
    }

    fn convert_ivf<R, W>(&self, reader: &mut R, writer: &mut W, options: &CliOptions) -> Result<()>
    where
        R: Read,
        W: Write,
    {
        loop {
            let fh: IvfFrameHeader = match read_ivf_frame_header(reader)? {
                Some(h) => h,
                None => break,
            };

            let mut frame_data = vec![0u8; fh.frame_size as usize];
            reader.read_exact(&mut frame_data)?;

            let obus = read_obus_from_ivf_frame(frame_data)?;
            let output_frame = Self::convert_frame(&obus, options)?;

            write_ivf_frame_header(writer, output_frame.len() as u32, fh.timestamp)?;
            writer.write_all(&output_frame)?;
        }

        Ok(())
    }

    fn convert_raw<R, W>(&self, reader: &mut R, writer: &mut W, options: &CliOptions) -> Result<()>
    where
        R: Read,
        W: Write,
    {
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
                // Flush current TU
                let td = current_td.take().unwrap();
                writer.write_all(&td.raw_bytes)?;

                for obu in pending.drain(..) {
                    if let Some(t35) = extract_dovi_t35_payload(&obu.payload) {
                        let mut rpu = DoviRpu::parse_itu_t35_dovi_metadata_obu(t35)?;
                        convert_rpu_with_opts(options, &mut rpu)?;
                        let new_obu = build_dovi_obu(&rpu)?;
                        writer.write_all(&new_obu)?;
                    } else {
                        writer.write_all(&obu.raw_bytes)?;
                    }
                }
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
                        writer.write_all(&obu.raw_bytes)?;
                    }
                }
            }
        }

        Ok(())
    }

    fn convert_frame(obus: &[Obu], options: &CliOptions) -> Result<Vec<u8>> {
        let mut out = Vec::new();

        for obu in obus {
            if is_dovi_rpu_obu(obu) {
                let t35 = extract_dovi_t35_payload(&obu.payload).unwrap();
                let mut rpu = DoviRpu::parse_itu_t35_dovi_metadata_obu(t35)?;
                convert_rpu_with_opts(options, &mut rpu)?;
                let new_obu = build_dovi_obu(&rpu)?;
                out.extend_from_slice(&new_obu);
            } else {
                out.extend_from_slice(&obu.raw_bytes);
            }
        }

        Ok(out)
    }
}
