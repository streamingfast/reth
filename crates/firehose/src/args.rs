//! CLI arguments for the Firehose integration.
//!
//! These arguments control how the Firehose tracer emits blocks to stdout,
//! where to write the cursor file, and how async emission is configured.

use clap::Args;
use firehose_tracer::EmissionMode;
use std::{path::PathBuf, time::Duration};

/// Firehose emission mode, mirroring [`EmissionMode`] for CLI parsing.
#[derive(Debug, Clone, Default, clap::ValueEnum)]
pub enum EmissionModeArg {
    /// Encode and write blocks inline on the calling thread (legacy behaviour).
    Blocking,
    /// Encode and write blocks in a dedicated background thread with backpressure.
    Async,
    /// Switch automatically based on block age (catch-up → async, live → blocking).
    #[default]
    Auto,
}

/// CLI arguments for the Firehose tracer integration.
///
/// Add `#[command(flatten)]` to include these in a `NodeCommand` extension struct.
#[derive(Debug, Clone, Default, Args)]
pub struct FirehoseArgs {
    /// Controls when and how encoded blocks are written to stdout.
    ///
    /// - `blocking`: encode → base64 → write, all inline on the calling thread (legacy).
    /// - `async`:    encode and write in a background thread; backpressure via channel.
    /// - `auto`:     use async for blocks older than `--firehose.live-threshold`; use blocking for
    ///   blocks within the live window (default).
    #[arg(
        id = "firehose.emission-mode",
        long = "firehose.emission-mode",
        value_name = "MODE",
        default_value = "auto",
        verbatim_doc_comment
    )]
    pub emission_mode: EmissionModeArg,

    /// Channel capacity for the async emission path.
    ///
    /// The background writer thread will block producers once this many encoded
    /// blocks are waiting, providing backpressure. Only relevant for `async` and
    /// `auto` modes.
    #[arg(
        id = "firehose.channel-capacity",
        long = "firehose.channel-capacity",
        value_name = "N",
        default_value_t = 32
    )]
    pub channel_capacity: usize,

    /// Age threshold in seconds used by `auto` emission mode.
    ///
    /// Blocks with a timestamp more than this many seconds behind wall-clock time
    /// are considered historical (catch-up) and will use the async path.
    /// Blocks within this window are considered live and will use the blocking path.
    #[arg(
        id = "firehose.live-threshold",
        long = "firehose.live-threshold",
        value_name = "SECS",
        default_value_t = 60
    )]
    pub live_threshold_secs: u64,

    /// Path to the cursor file that tracks the last block successfully emitted to stdout.
    ///
    /// After each block is written the cursor file is updated atomically so that the
    /// node can detect gaps after an unclean shutdown and re-emit the missing blocks
    /// on the next startup. Defaults to `<datadir>/firehose.cursor` when not set.
    #[arg(id = "firehose.cursor-path", long = "firehose.cursor-path", value_name = "PATH")]
    pub cursor_path: Option<PathBuf>,
}

impl FirehoseArgs {
    /// Convert the parsed CLI args into a [`firehose_tracer::config::Config`].
    ///
    /// `data_dir` is used to derive the default cursor file path when
    /// `--firehose.cursor-path` is not specified.
    pub fn to_tracer_config(&self, data_dir: &std::path::Path) -> firehose_tracer::config::Config {
        let cursor_path =
            self.cursor_path.clone().unwrap_or_else(|| data_dir.join("firehose.cursor"));

        let emission_mode = match self.emission_mode {
            EmissionModeArg::Blocking => EmissionMode::Blocking,
            EmissionModeArg::Async => {
                EmissionMode::Async { channel_capacity: self.channel_capacity }
            }
            EmissionModeArg::Auto => EmissionMode::Auto {
                channel_capacity: self.channel_capacity,
                live_threshold: Duration::from_secs(self.live_threshold_secs),
            },
        };

        firehose_tracer::config::Config::new()
            .with_emission_mode(emission_mode)
            .with_cursor_path(cursor_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_emission_mode_is_auto() {
        let args = FirehoseArgs::default();
        assert!(matches!(args.emission_mode, EmissionModeArg::Auto));
    }

    #[test]
    fn to_tracer_config_uses_data_dir_for_cursor() {
        let args = FirehoseArgs::default();
        let tmp = tempfile::tempdir().unwrap();
        let cfg = args.to_tracer_config(tmp.path());
        let expected_cursor = tmp.path().join("firehose.cursor");
        assert_eq!(cfg.cursor_path, Some(expected_cursor));
    }

    #[test]
    fn to_tracer_config_respects_explicit_cursor_path() {
        let custom = PathBuf::from("/custom/firehose.cursor");
        let args = FirehoseArgs { cursor_path: Some(custom.clone()), ..Default::default() };
        let tmp = tempfile::tempdir().unwrap();
        let cfg = args.to_tracer_config(tmp.path());
        assert_eq!(cfg.cursor_path, Some(custom));
    }
}
