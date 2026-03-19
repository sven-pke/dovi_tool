use clap::Parser;

use dolby_vision::rpu::ConversionMode;

mod convert;
mod editor;
mod export;
mod extract_rpu;
pub(crate) mod generate;
mod info;
mod inject_rpu;
mod plot;
mod remove;

pub use convert::ConvertArgs;
pub use editor::EditorArgs;
pub use export::{ExportArgs, ExportData};
pub use extract_rpu::ExtractRpuArgs;
pub use generate::GenerateArgs;
pub use info::InfoArgs;
pub use inject_rpu::InjectRpuArgs;
pub use plot::PlotArgs;
pub use remove::RemoveArgs;

#[derive(Parser, Debug)]
pub enum Commands {
    #[command(about = "Converts the Dolby Vision RPU within an AV1 bitstream")]
    Convert(ConvertArgs),

    #[command(about = "Edits a binary RPU according to a JSON config")]
    Editor(EditorArgs),

    #[command(about = "Exports a binary RPU file to JSON for simpler analysis")]
    Export(ExportArgs),

    #[command(about = "Extracts Dolby Vision RPU from an AV1 bitstream")]
    ExtractRpu(ExtractRpuArgs),

    #[command(about = "Interleaves RPU OBUs into an AV1 bitstream")]
    InjectRpu(InjectRpuArgs),

    #[command(about = "Generates a binary RPU from different sources")]
    Generate(GenerateArgs),

    #[command(about = "Prints the parsed RPU data as JSON for a specific frame")]
    Info(InfoArgs),

    #[command(about = "Plot the L1/L2/L8 metadata")]
    Plot(PlotArgs),

    #[command(about = "Removes Dolby Vision RPU OBUs from an AV1 bitstream")]
    Remove(RemoveArgs),
}

#[derive(clap::ValueEnum, Debug, Copy, Clone)]
pub enum ConversionModeCli {
    #[value(name = "0")]
    Lossless = 0,
    #[value(name = "1")]
    ToMel,
    #[value(name = "2")]
    To81,
    #[value(name = "3")]
    Profile5To81,
    #[value(name = "4")]
    To84,
    #[value(name = "5")]
    To81MappingPreserved,
}

impl From<ConversionModeCli> for ConversionMode {
    fn from(mode: ConversionModeCli) -> ConversionMode {
        match mode {
            ConversionModeCli::Lossless => ConversionMode::Lossless,
            ConversionModeCli::ToMel => ConversionMode::ToMel,
            ConversionModeCli::To81 | ConversionModeCli::Profile5To81 => ConversionMode::To81,
            ConversionModeCli::To84 => ConversionMode::To84,
            ConversionModeCli::To81MappingPreserved => ConversionMode::To81MappingPreserved,
        }
    }
}
