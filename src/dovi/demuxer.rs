use anyhow::Result;
use indicatif::ProgressBar;
use std::path::PathBuf;

use crate::{commands::DemuxArgs, dovi::general_read_write::DoviProcessorError};

use super::{CliOptions, IoFormat, general_read_write, input_from_either};

use general_read_write::{DoviProcessor, DoviWriter};

pub struct Demuxer {
    format: IoFormat,
    input: PathBuf,
    bl_out: PathBuf,
    el_out: PathBuf,
    el_only: bool,
}

impl Demuxer {
    pub fn from_args(args: DemuxArgs) -> Result<Self> {
        let DemuxArgs {
            input,
            input_pos,
            bl_out,
            el_out,
            el_only,
        } = args;

        let input = input_from_either("demux", input, input_pos)?;
        let format = hevc_parser::io::format_from_path(&input)?;

        let bl_out = match bl_out {
            Some(path) => path,
            None => PathBuf::from("BL.hevc"),
        };

        let el_out = match el_out {
            Some(path) => path,
            None => PathBuf::from("EL.hevc"),
        };

        Ok(Self {
            format,
            input,
            bl_out,
            el_out,
            el_only,
        })
    }

    pub fn demux(args: DemuxArgs, options: CliOptions) -> Result<()> {
        let demuxer = Demuxer::from_args(args)?;
        demuxer.process_input(options)
    }

    fn process_input(&self, options: CliOptions) -> Result<()> {
        let pb = super::initialize_progress_bar(&self.format, &self.input)?;

        if self.format == IoFormat::Matroska {
            println!("Demuxer: Matroska input is experimental!");
        }

        self.demux_hevc(pb, options)
    }

    fn demux_hevc(&self, pb: ProgressBar, options: CliOptions) -> Result<()> {
        let bl_out = if self.el_only {
            None
        } else {
            Some(self.bl_out.as_path())
        };
        let el_out = self.el_out.as_path();

        let dovi_writer = DoviWriter::new(bl_out, Some(el_out), None, None);
        let mut dovi_processor = DoviProcessor::new(
            options,
            self.input.clone(),
            dovi_writer,
            pb,
            Default::default(),
        );

        let res = dovi_processor.read_write_from_io(&self.format);

        if res.as_ref().is_err_and(|err| {
            err.downcast_ref::<DoviProcessorError>()
                .is_some_and(|e| matches!(e, DoviProcessorError::NoElFound))
        }) {
            if let Some(bl_out) = bl_out {
                std::fs::remove_file(bl_out)?;
            }
            std::fs::remove_file(el_out)?;
        }

        res
    }
}
