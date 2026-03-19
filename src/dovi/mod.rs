use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::{fs::File, io::BufWriter};

use anyhow::{Result, bail};
use indicatif::{ProgressBar, ProgressStyle};

use dolby_vision::rpu::dovi_rpu::DoviRpu;

use self::editor::EditConfig;
use super::commands::ConversionModeCli;

pub mod av1_parser;
pub mod converter;
pub mod editor;
pub mod exporter;
pub mod generator;
pub mod plotter;
pub mod remover;
pub mod rpu_extractor;
pub mod rpu_info;
pub mod rpu_injector;

#[derive(Debug, Clone)]
pub struct CliOptions {
    pub mode: Option<ConversionModeCli>,
    pub crop: bool,
    pub edit_config: Option<EditConfig>,
}

pub fn initialize_progress_bar<P: AsRef<Path>>(input: P) -> Result<ProgressBar> {
    let file = File::open(input).expect("No file found");
    let file_meta = file.metadata()?;
    let bytes_count = file_meta.len() / 100_000_000;

    let pb = ProgressBar::new(bytes_count);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:60.cyan} {percent}%")?,
    );

    Ok(pb)
}

/// Apply conversion mode / crop / edit_config to a single RPU in place.
pub fn convert_rpu_with_opts(opts: &CliOptions, rpu: &mut DoviRpu) -> Result<()> {
    if let Some(edit_config) = &opts.edit_config {
        edit_config.execute_single_rpu(rpu)?;
    } else {
        if let Some(mode) = opts.mode {
            rpu.convert_with_mode(mode)?;
        }
        if opts.crop {
            rpu.crop()?;
        }
    }
    Ok(())
}

/// Write an RPU binary file.
///
/// Each entry in `data` is the output of `write_hevc_unspec62_nalu()`:
/// `[0x7C, 0x01, <RPU bytes>]`.
/// This function writes them as 4-byte-start-code NAL units for compatibility
/// with `parse_rpu_file` and other Dolby Vision tooling.
pub fn write_rpu_file<P: AsRef<Path>>(output_path: P, data: Vec<Vec<u8>>) -> Result<()> {
    let mut writer = BufWriter::with_capacity(
        100_000,
        File::create(output_path.as_ref()).expect("Can't create RPU file"),
    );

    for encoded_rpu in &data {
        // Write 4-byte start code followed by the NAL data (0x7C 0x01 + RPU)
        writer.write_all(&[0x00, 0x00, 0x00, 0x01])?;
        writer.write_all(encoded_rpu)?;
    }

    writer.flush()?;
    Ok(())
}

pub fn input_from_either(cmd: &str, in1: Option<PathBuf>, in2: Option<PathBuf>) -> Result<PathBuf> {
    match in1 {
        Some(in1) => Ok(in1),
        None => match in2 {
            Some(in2) => Ok(in2),
            None => bail!("No input file provided. See `dovi_tool {} --help`", cmd),
        },
    }
}
