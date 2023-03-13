use {
    solana_rbpf::{
        ebpf::Insn,
        error::EbpfError,
        static_analysis::{Analysis, CfgNode, TraceLogEntry},
        vm::Config,
    },
    solana_sdk::{hash::Hash, pubkey::Pubkey},
    std::{
        any::Any,
        collections::BTreeMap,
        sync::Arc,
        {error, io},
    },
    thiserror::Error,
};

/// Errors returned by plugin calls
#[derive(Error, Debug)]
pub enum BpfTracerPluginError {
    /// Error opening the configuration file; for example, when the file
    /// is not found or when the validator process has no permission to read it.
    #[error("Error opening config file. Error detail: ({0}).")]
    ConfigFileOpenError(#[from] io::Error),

    /// Error in reading the content of the config file or the content
    /// is not in the expected format.
    #[error("Error reading config file. Error message: ({msg})")]
    ConfigFileReadError { msg: String },

    /// Any custom error defined by the plugin.
    #[error("Plugin-defined custom error. Error message: ({0})")]
    Custom(Box<dyn error::Error + Send + Sync>),
}

/// Additional methods for program executor
pub trait ExecutorAdditional: Send + Sync {
    /// Performs static analysis of current verified executable
    fn do_static_analysis(&self) -> std::result::Result<Analysis, EbpfError>;
    /// Calculates `text` section offset of current executable
    fn get_text_section_offset(&self) -> u64;
    /// Gets program counter of function by its hash
    fn lookup_internal_function(&self, hash: u32) -> Option<usize>;
    /// Gets executable configuration
    fn get_config(&self) -> &Config;
    /// Disassembles instruction
    fn disassemble_instruction(
        &self,
        ebpf_instr: &Insn,
        cfg_nodes: &BTreeMap<usize, CfgNode>,
    ) -> String;
}

pub type Result<T> = std::result::Result<T, BpfTracerPluginError>;

pub trait BpfTracerPlugin: Any + Send + Sync + std::fmt::Debug {
    fn name(&self) -> &'static str;

    /// The callback called when a plugin is loaded by the system, used for doing
    /// whatever initialization is required by the plugin. The _config_file
    /// contains the name of the config file. The config must be in JSON format
    /// and include a field "libpath" indicating the full path name of the shared
    /// library implementing this interface.
    #[allow(unused_variables)]
    fn on_load(&mut self, config_file: &str) -> Result<()> {
        Ok(())
    }

    /// The callback called right before a plugin is unloaded by the system
    /// Used for doing cleanup before unload.
    fn on_unload(&mut self) {}

    /// Check if the plugin is accepting BPF tracing.
    ///
    /// Note: This associated function is expected to return as soon as possible in order to
    /// not affect validator's performance.
    fn bpf_tracing_enabled(&self) -> bool {
        true
    }

    /// Called when BPF trace is ready.
    ///
    /// Note: This associated function is expected to return as soon as possible in order to
    /// not affect validator's performance.
    fn trace_bpf(
        &mut self,
        program_id: &Pubkey,
        block_hash: &Hash,
        transaction_id: &[u8],
        trace: &[TraceLogEntry],
        consumed_bpf_units: &[(usize, u64)],
        executor: Arc<dyn ExecutorAdditional>,
    ) -> Result<()>;
}
