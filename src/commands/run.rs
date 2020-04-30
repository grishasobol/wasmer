use crate::common::get_cache_dir;
use crate::compiler::CompilerOptions;
use anyhow::{anyhow, bail, Context, Result};
use std::path::PathBuf;
use std::str::FromStr;
use wasmer::*;
#[cfg(feature = "cache")]
use wasmer_cache::{Cache, FileSystemCache, WasmHash};

use structopt::StructOpt;

#[cfg(feature = "wasi")]
mod wasi;

#[cfg(feature = "wasi")]
use wasi::Wasi;

#[derive(Debug, StructOpt, Clone)]
/// The options for the `wasmer run` subcommand
pub struct Run {
    /// Disable the cache
    #[structopt(long = "disable-cache")]
    disable_cache: bool,

    /// Input file
    #[structopt(parse(from_os_str))]
    path: PathBuf,

    /// Invoke a specified function
    #[structopt(long = "invoke", short = "i")]
    invoke: Option<String>,

    /// The command name is a string that will override the first argument passed
    /// to the wasm program. This is used in wapm to provide nicer output in
    /// help commands and error messages of the running wasm program
    #[structopt(long = "command-name", hidden = true)]
    command_name: Option<String>,

    /// A prehashed string, used to speed up start times by avoiding hashing the
    /// wasm module. If the specified hash is not found, Wasmer will hash the module
    /// as if no `cache-key` argument was passed.
    #[structopt(long = "cache-key", hidden = true)]
    cache_key: Option<String>,

    #[structopt(flatten)]
    compiler: CompilerOptions,

    #[cfg(feature = "wasi")]
    #[structopt(flatten)]
    wasi: Wasi,

    /// Enable non-standard experimental IO devices
    #[cfg(feature = "io-devices")]
    #[structopt(long = "enable-io-devices")]
    enable_experimental_io_devices: bool,

    /// Enable debug output
    #[cfg(feature = "debug")]
    #[structopt(long = "debug", short = "d")]
    debug: bool,

    /// Application arguments
    #[structopt(name = "--", multiple = true)]
    args: Vec<String>,
}

impl Run {
    #[cfg(feature = "cache")]
    /// Get the Compiler Filesystem cache
    fn get_cache(&self, compiler_name: String) -> Result<FileSystemCache> {
        let mut cache_dir_root = get_cache_dir();
        cache_dir_root.push(compiler_name);
        Ok(FileSystemCache::new(cache_dir_root)?)
    }

    /// Execute the run command
    pub fn execute(&self) -> Result<()> {
        let (compiler_config, compiler_name) = self.compiler.get_config()?;
        let engine = Engine::new(&*compiler_config);
        let store = Store::new(&engine);
        // We try to get it from cache, in case is enabled
        let module = if cfg!(feature = "cache") && !self.disable_cache {
            let mut cache = self.get_cache(compiler_name)?;
            let contents = std::fs::read(self.path.clone())?;
            // Try to get the hash from the provided `--cache-key`, otherwise
            // generate one from the provided file `.wasm` contents.
            let hash = self
                .cache_key
                .as_ref()
                .and_then(|key| WasmHash::from_str(&key).ok())
                .unwrap_or(WasmHash::generate(&contents));
            let module = match unsafe { cache.load(&store, hash) } {
                Ok(module) => module,
                Err(_e) => {
                    let module = Module::from_binary(&store, &contents)?;
                    // Store the compiled Module in cache
                    cache.store(hash, module.clone());
                    module
                }
            };
            module
        } else {
            Module::from_file(&store, &self.path.clone())?
        };

        // Do we want to invoke a function?
        if let Some(ref invoke) = self.invoke {
            let imports = imports! {};
            let instance = Instance::new(&module, &imports)?;
            let result = self
                .invoke_function(&instance, &invoke, &self.args)
                .with_context(|| format!("Failed to invoke `{}`", invoke))?;
            println!(
                "{}",
                result
                    .iter()
                    .map(|val| val.to_string())
                    .collect::<Vec<String>>()
                    .join(" ")
            );
            return Ok(());
        }

        // If WASI is enabled, try to execute it with it
        if cfg!(feature = "wasi") {
            let wasi_version = Wasi::get_version(&module);
            if let Some(version) = wasi_version {
                let program_name = self
                    .command_name
                    .clone()
                    .or_else(|| {
                        self.path
                            .file_name()
                            .map(|f| f.to_string_lossy().to_string())
                    })
                    .unwrap_or("".to_string());
                self.wasi
                    .execute(module, version, program_name, self.args.clone())?;
            }
        } else {
            // Try to instantiate the wasm file, with no provided imports
            let imports = imports! {};
            let instance = Instance::new(&module, &imports)?;
            let start: &Func = instance.exports.get("_start")?;
            start.call(&[])?;
        }

        Ok(())
    }

    fn invoke_function(
        &self,
        instance: &Instance,
        invoke: &str,
        args: &Vec<String>,
    ) -> Result<Box<[Val]>> {
        let func: &Func = instance.exports.get(&invoke)?;
        let func_ty = func.ty();
        let required_arguments = func_ty.params().len();
        let provided_arguments = args.len();
        if required_arguments != provided_arguments {
            bail!(
                "Function expected {} arguments, but received {}: \"{}\"",
                required_arguments,
                provided_arguments,
                self.args.join(" ")
            );
        }
        let invoke_args = args
            .iter()
            .zip(func_ty.params().iter())
            .map(|(arg, param_type)| match param_type {
                ValType::I32 => {
                    Ok(Val::I32(arg.parse().map_err(|_| {
                        anyhow!("Can't convert `{}` into a i32", arg)
                    })?))
                }
                ValType::I64 => {
                    Ok(Val::I64(arg.parse().map_err(|_| {
                        anyhow!("Can't convert `{}` into a i64", arg)
                    })?))
                }
                ValType::F32 => {
                    Ok(Val::F32(arg.parse().map_err(|_| {
                        anyhow!("Can't convert `{}` into a f32", arg)
                    })?))
                }
                ValType::F64 => {
                    Ok(Val::F64(arg.parse().map_err(|_| {
                        anyhow!("Can't convert `{}` into a f64", arg)
                    })?))
                }
                _ => Err(anyhow!(
                    "Don't know how to convert {} into {:?}",
                    arg,
                    param_type
                )),
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(func.call(&invoke_args)?)
    }
}
