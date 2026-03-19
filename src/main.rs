use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, ValueHint};

#[cfg(test)]
mod tests;

mod commands;
use commands::{Commands, ConversionModeCli};

mod dovi;
use dovi::{
    CliOptions,
    converter::Converter,
    editor::{EditConfig, Editor},
    exporter::Exporter,
    generator::Generator,
    plotter::Plotter,
    remover::Remover,
    rpu_extractor::RpuExtractor,
    rpu_info::RpuInfo,
    rpu_injector::RpuInjector,
};

#[derive(Parser, Debug)]
#[command(
    name = env!("CARGO_PKG_NAME"),
    about = "CLI tool combining multiple utilities for working with Dolby Vision in AV1",
    author = "quietvoid",
    version = option_env!("VERGEN_GIT_DESCRIBE").unwrap_or(env!("CARGO_PKG_VERSION"))
)]
struct Opt {
    #[arg(
        id = "mode",
        short = 'm',
        long,
        help = "Sets the mode for RPU processing. See --help for more info",
        long_help = "Sets the mode for RPU processing.\n  \
                     Mode 0: Parses the RPU, rewrites it untouched\n  \
                     Mode 1: Converts the RPU to be MEL compatible\n  \
                     Mode 2: Converts the RPU to be profile 8.1 compatible. Removes mapping\n  \
                     Mode 3: Converts profile 5 to 8.1\n  \
                     Mode 4: Converts to profile 8.4\n  \
                     Mode 5: Converts to profile 8.1, preserving luma/chroma mapping",
        value_enum
    )]
    mode: Option<ConversionModeCli>,

    #[arg(
        long,
        short = 'c',
        help = "Set active area offsets to 0 (meaning no letterbox bars)"
    )]
    crop: bool,

    #[arg(
        long,
        help = "Sets the edit JSON config file to use",
        value_hint = ValueHint::FilePath
    )]
    edit_config: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Commands,
}

fn main() -> Result<()> {
    let opt = Opt::parse();

    let edit_config = opt
        .edit_config
        .as_ref()
        .map(EditConfig::from_path)
        .and_then(Result::ok);

    let mut cli_options = CliOptions {
        mode: opt.mode,
        crop: opt.crop,
        edit_config,
    };

    // Set mode 0 by default if cropping, otherwise it has no effect
    if cli_options.mode.is_none() && cli_options.crop {
        cli_options.mode = Some(ConversionModeCli::Lossless);
    }

    match opt.cmd {
        Commands::Convert(args) => Converter::convert(args, cli_options),
        Commands::Editor(args) => Editor::edit(args),
        Commands::Export(args) => Exporter::export(args),
        Commands::ExtractRpu(args) => RpuExtractor::extract_rpu(args, cli_options),
        Commands::InjectRpu(args) => RpuInjector::inject_rpu(args, cli_options),
        Commands::Info(args) => RpuInfo::info(args),
        Commands::Generate(args) => Generator::generate(args),
        Commands::Plot(args) => Plotter::plot(args),
        Commands::Remove(args) => Remover::remove(args, cli_options),
    }
}
