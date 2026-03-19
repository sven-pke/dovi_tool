use std::fs::File;
use std::io::{BufReader, BufWriter, Write, stdout};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use indicatif::ProgressBar;

use hevc_parser::HevcParser;
use hevc_parser::io::{FrameBuffer, IoProcessor, NalBuffer, processor};
use hevc_parser::{NALUStartCode, hevc::*};
use processor::{HevcProcessor, HevcProcessorOpts};

use dolby_vision::rpu::utils::parse_rpu_file;

use crate::commands::InjectRpuArgs;

use super::av1::{
    IvfWriter, ObuReader, ObuWriter,
    build_dovi_obu, is_dovi_rpu_obu,
    try_read_ivf_file_header, read_ivf_frame_header, read_obus_from_ivf_frame,
};
use super::hdr10plus_utils::prefix_sei_removed_hdr10plus_nalu;
use super::{CliOptions, DoviRpu, IoFormat, input_from_either};

fn is_av1_input(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("av1") | Some("ivf")
    )
}

fn inject_rpu_av1(input: &Path, rpu_in: &Path, output: &Path) -> Result<()> {
    println!("Parsing RPU file...");
    stdout().flush().ok();

    let rpus = parse_rpu_file(rpu_in)?;

    println!("Injecting RPU into AV1 bitstream...");
    stdout().flush().ok();

    let in_file = File::open(input)?;
    let mut reader = BufReader::new(in_file);

    if let Some(ivf_header) = try_read_ivf_file_header(&mut reader)? {
        // IVF container path
        let out_file = BufWriter::new(File::create(output).expect("Can't create file"));
        let mut ivf_writer = IvfWriter::new(out_file, &ivf_header)?;

        let mut frame_index = 0usize;
        while let Some(frame_hdr) = read_ivf_frame_header(&mut reader)? {
            let mut frame_data = vec![0u8; frame_hdr.frame_size as usize];
            std::io::Read::read_exact(&mut reader, &mut frame_data)?;

            let obus = read_obus_from_ivf_frame(frame_data)?;
            let mut new_frame: Vec<u8> = Vec::new();

            // Build the DoVi OBU for this frame
            let rpu_obu_bytes = if let Some(rpu) = rpus.get(frame_index).or_else(|| rpus.last()) {
                Some(build_dovi_obu(rpu)?)
            } else {
                None
            };

            let mut rpu_inserted = false;
            for obu in &obus {
                if is_dovi_rpu_obu(obu) {
                    // Replace existing DoVi OBU with new one
                    if let Some(ref bytes) = rpu_obu_bytes {
                        if !rpu_inserted {
                            new_frame.extend_from_slice(bytes);
                            rpu_inserted = true;
                        }
                    }
                } else {
                    new_frame.extend_from_slice(&obu.raw_bytes);
                }
            }

            // If no existing DoVi OBU was found, append the new one at end
            if !rpu_inserted {
                if let Some(ref bytes) = rpu_obu_bytes {
                    new_frame.extend_from_slice(bytes);
                }
            }

            ivf_writer.write_frame(frame_hdr.timestamp, &new_frame)?;
            frame_index += 1;
        }

        ivf_writer.flush()?;
    } else {
        // Raw AV1 bitstream path
        let out_file = BufWriter::new(File::create(output).expect("Can't create file"));
        let mut obu_writer = ObuWriter::new(out_file);
        let mut obu_reader = ObuReader::new(reader);

        let mut frame_index = 0usize;
        while let Some(obu) = obu_reader.next_obu()? {
            if is_dovi_rpu_obu(&obu) {
                // Replace with new DoVi OBU
                if let Some(rpu) = rpus.get(frame_index).or_else(|| rpus.last()) {
                    let bytes = build_dovi_obu(rpu)?;
                    obu_writer.write_raw(&bytes)?;
                    frame_index += 1;
                }
            } else {
                obu_writer.write_raw(&obu.raw_bytes)?;
            }
        }

        obu_writer.flush()?;
    }

    println!("Done.");
    Ok(())
}

pub struct RpuInjector {
    input: PathBuf,
    rpu_in: PathBuf,
    no_add_aud: bool,
    options: CliOptions,

    rpus: Vec<DoviRpu>,

    writer: BufWriter<File>,
    progress_bar: ProgressBar,
    already_checked_for_rpu: bool,

    frames: Vec<Frame>,
    nals: Vec<NALUnit>,
    mismatched_length: bool,

    frame_buffer: FrameBuffer,
}

impl RpuInjector {
    pub fn from_args(args: InjectRpuArgs, cli_options: CliOptions) -> Result<RpuInjector> {
        let InjectRpuArgs {
            input,
            input_pos,
            rpu_in,
            output,
            no_add_aud,
        } = args;

        let input = input_from_either("inject-rpu", input, input_pos)?;

        let output = match output {
            Some(path) => path,
            None => PathBuf::from("injected_output.hevc"),
        };

        let chunk_size = 100_000;
        let progress_bar = super::initialize_progress_bar(&IoFormat::Raw, &input)?;

        let writer =
            BufWriter::with_capacity(chunk_size, File::create(output).expect("Can't create file"));

        let mut injector = RpuInjector {
            input,
            rpu_in,
            no_add_aud,
            options: cli_options,
            rpus: Vec::new(),

            writer,
            progress_bar,
            already_checked_for_rpu: false,

            frames: Vec::new(),
            nals: Vec::new(),
            mismatched_length: false,

            frame_buffer: FrameBuffer {
                frame_number: 0,
                nals: Vec::with_capacity(16),
            },
        };

        println!("Parsing RPU file...");
        stdout().flush().ok();

        // Assumes parsing returns on error
        injector.rpus = parse_rpu_file(&injector.rpu_in)?;

        Ok(injector)
    }

    pub fn inject_rpu(args: InjectRpuArgs, cli_options: CliOptions) -> Result<()> {
        let input = input_from_either("inject-rpu", args.input.clone(), args.input_pos.clone())?;

        if is_av1_input(&input) {
            let output = args.output.clone().unwrap_or_else(|| {
                let ext = input.extension().and_then(|e| e.to_str()).unwrap_or("av1");
                PathBuf::from(format!("injected_output.{ext}"))
            });
            return inject_rpu_av1(&input, &args.rpu_in, &output);
        }

        let format = hevc_parser::io::format_from_path(&input)?;

        if let IoFormat::Raw = format {
            let mut injector = RpuInjector::from_args(args, cli_options)?;

            injector.process_input()?;
            injector.interleave_rpu_nals()
        } else {
            bail!("RpuInjector: Must be a raw HEVC bitstream file")
        }
    }

    fn process_input(&mut self) -> Result<()> {
        println!("Processing input video for frame order info...");
        stdout().flush().ok();

        let chunk_size = 100_000;

        let mut processor =
            HevcProcessor::new(IoFormat::Raw, HevcProcessorOpts::default(), chunk_size);

        let file = File::open(&self.input)?;
        let mut reader = Box::new(BufReader::with_capacity(100_000, file));

        processor.process_io(&mut reader, self)
    }

    fn interleave_rpu_nals(&mut self) -> Result<()> {
        let rpus = &self.rpus;

        self.mismatched_length = if self.frames.len() != rpus.len() {
            println!(
                "\nWarning: mismatched lengths. video {}, RPU {}",
                self.frames.len(),
                rpus.len()
            );

            if rpus.len() < self.frames.len() {
                println!("Metadata will be duplicated at the end to match video length\n");
            } else {
                println!("Metadata will be skipped at the end to match video length\n");
            }

            true
        } else {
            false
        };

        println!("Rewriting file with interleaved RPU NALs..");
        stdout().flush().ok();

        self.progress_bar = super::initialize_progress_bar(&IoFormat::Raw, &self.input)?;

        let chunk_size = 100_000;

        let mut processor =
            HevcProcessor::new(IoFormat::Raw, HevcProcessorOpts::default(), chunk_size);

        let file = File::open(&self.input)?;
        let mut reader = Box::new(BufReader::with_capacity(chunk_size, file));

        processor.process_io(&mut reader, self)
    }

    fn get_rpu_and_index_to_insert(
        frames: &[Frame],
        rpus: &[DoviRpu],
        frame_buffer: &FrameBuffer,
        mismatched_length: bool,
    ) -> Result<(usize, NalBuffer)> {
        let existing_frame = frames
            .iter()
            .find(|f| f.decoded_number == frame_buffer.frame_number);

        // If we have a RPU buffered frame, write it
        // Otherwise, write the same data as previous
        let rpu_nb = if let Some(frame) = existing_frame {
            let dovi_rpu = rpus
                .get(frame.presentation_number as usize)
                .or_else(|| mismatched_length.then(|| rpus.last()).flatten());

            if let Some(rpu) = dovi_rpu {
                let data = rpu.write_hevc_unspec62_nalu()?;

                Some(NalBuffer {
                    nal_type: NAL_UNSPEC62,
                    start_code: NALUStartCode::Length4,
                    data,
                })
            } else {
                bail!(
                    "No RPU found for presentation frame {}",
                    frame.presentation_number
                );
            }
        } else if mismatched_length && let Some(rpu) = rpus.last() {
            let data = rpu.write_hevc_unspec62_nalu()?;

            Some(NalBuffer {
                nal_type: NAL_UNSPEC62,
                start_code: NALUStartCode::Length4,
                data,
            })
        } else {
            None
        };

        if let Some(rpu_nb) = rpu_nb {
            // Insert after the last NALU
            // FFmpeg always expects RPU to be the last
            let insert_index = frame_buffer.nals.len().checked_sub(1);

            if let Some(idx) = insert_index {
                // + 1 since we want the RPU after
                Ok((idx + 1, rpu_nb))
            } else {
                bail!(
                    "No slice or UNSPEC63 NALUs in decoded frame {}. Cannot insert RPU.",
                    frame_buffer.frame_number
                );
            }
        } else {
            bail!(
                "No RPU data to write for decoded frame {}",
                frame_buffer.frame_number
            );
        }
    }
}

impl IoProcessor for RpuInjector {
    fn input(&self) -> &PathBuf {
        &self.input
    }

    fn update_progress(&mut self, delta: u64) {
        if !self.already_checked_for_rpu {
            self.already_checked_for_rpu = true;
        }

        self.progress_bar.inc(delta);
    }

    fn process_nals(&mut self, _parser: &HevcParser, nals: &[NALUnit], chunk: &[u8]) -> Result<()> {
        // Second pass
        if !self.frames.is_empty() && !self.nals.is_empty() {
            let rpus = &self.rpus;

            for nal in nals {
                let mut nalu_data_override = None;

                // Ignore HDR10+
                if self.options.drop_hdr10plus && nal.nal_type == NAL_SEI_PREFIX {
                    let (has_st2094_40, data) = prefix_sei_removed_hdr10plus_nalu(chunk, nal)?;

                    // Drop NALUs containing only one SEI message
                    if has_st2094_40 && data.is_none() {
                        continue;
                    } else {
                        nalu_data_override = data;
                    }
                }

                if self.frame_buffer.frame_number != nal.decoded_frame_index {
                    // On new frame, write AUD
                    if !self.no_add_aud {
                        // Skip existing AUDs
                        if nal.nal_type == NAL_AUD {
                            continue;
                        }

                        if self.frame_buffer.frame_number != nal.decoded_frame_index {
                            // Find existing frame for the current buffered frame
                            let buffered_frame = self
                                .frames
                                .iter()
                                .find(|f| f.decoded_number == self.frame_buffer.frame_number)
                                .unwrap();

                            self.frame_buffer.nals.insert(
                                0,
                                NalBuffer {
                                    nal_type: NAL_AUD,
                                    start_code: NALUStartCode::Length4,
                                    data: hevc_parser::utils::aud_for_frame(buffered_frame, None)?,
                                },
                            );
                        }
                    }

                    let (idx, rpu_nb) = Self::get_rpu_and_index_to_insert(
                        &self.frames,
                        rpus,
                        &self.frame_buffer,
                        self.mismatched_length,
                    )?;

                    self.frame_buffer.nals.insert(idx, rpu_nb);

                    // Write NALUs for the frame
                    for (i, nal_buf) in self.frame_buffer.nals.iter().enumerate() {
                        let first_nal = i == 0;

                        NALUnit::write_with_preset(
                            &mut self.writer,
                            &nal_buf.data,
                            self.options.start_code,
                            nal_buf.nal_type,
                            first_nal,
                        )?;
                    }

                    self.frame_buffer.frame_number = nal.decoded_frame_index;
                    self.frame_buffer.nals.clear();
                }

                // Ignore existing RPU
                if nal.nal_type != NAL_UNSPEC62 {
                    // Skip AUD NALUs if we're adding them
                    if !self.no_add_aud && nal.nal_type == NAL_AUD {
                        continue;
                    }

                    // Override in case of modified multi-message SEI
                    let final_chunk_data = if let Some(data) = nalu_data_override {
                        data
                    } else {
                        chunk[nal.start..nal.end].to_vec()
                    };

                    self.frame_buffer.nals.push(NalBuffer {
                        nal_type: nal.nal_type,
                        start_code: nal.start_code,
                        data: final_chunk_data,
                    });
                }
            }
        } else if !self.already_checked_for_rpu && nals.iter().any(|e| e.nal_type == NAL_UNSPEC62) {
            self.already_checked_for_rpu = true;
            println!("\nWarning: Input file already has RPUs, they will be replaced.");
        }

        Ok(())
    }

    fn finalize(&mut self, parser: &HevcParser) -> Result<()> {
        // First pass
        if self.frames.is_empty() && self.nals.is_empty() {
            self.frames.clone_from(parser.ordered_frames());
            self.nals.clone_from(parser.get_nals());
        } else {
            let ordered_frames = parser.ordered_frames();
            let total_frames = ordered_frames.len();

            // Last slice wasn't considered (no AUD/EOS NALU at the end)
            if (self.frame_buffer.frame_number as usize) != total_frames
                && !self.frame_buffer.nals.is_empty()
            {
                let rpus = &self.rpus;

                if !self.no_add_aud {
                    let last_frame = ordered_frames
                        .iter()
                        .find(|f| f.decoded_number == self.frame_buffer.frame_number)
                        .unwrap();

                    self.frame_buffer.nals.insert(
                        0,
                        NalBuffer {
                            nal_type: NAL_AUD,
                            start_code: NALUStartCode::Length4,
                            data: hevc_parser::utils::aud_for_frame(last_frame, None)?,
                        },
                    );
                }

                let (idx, rpu_nb) = Self::get_rpu_and_index_to_insert(
                    &self.frames,
                    rpus,
                    &self.frame_buffer,
                    self.mismatched_length,
                )?;

                self.frame_buffer.nals.insert(idx, rpu_nb);

                // Write NALUs for the last frame
                for (i, nal_buf) in self.frame_buffer.nals.iter().enumerate() {
                    let first_nal = i == 0;

                    NALUnit::write_with_preset(
                        &mut self.writer,
                        &nal_buf.data,
                        self.options.start_code,
                        nal_buf.nal_type,
                        first_nal,
                    )?;
                }

                self.frame_buffer.nals.clear();
            }

            // Second pass
            self.writer.flush()?;
        }

        self.progress_bar.finish_and_clear();

        Ok(())
    }
}
