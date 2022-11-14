
use wasmer::{Store, Module, Instance, Imports};
use wasmer_wasi::{WasiFunctionEnv, WasiBidirectionalSharedPipePair, WasiState};
use wasmer_vfs::{FileSystem, mem_fs::FileSystem as MemFileSystem};
use wasmer::{namespace, AsStoreMut, FunctionEnv, Function, Memory32, Memory64, Exports, MemorySize};
use wasmer_wasi::WasiEnv;
use std::path::{Path, PathBuf};
use std::collections::BTreeMap;
use wasmer::{FunctionEnvMut, WasmPtr};
use wasmer_wasi::types::wasi::Errno;

static TESSERACT_WASM: &[u8] = include_bytes!("../tesseract-core.wasm");
static TRAINED_DATA: &[u8] = include_bytes!("../eng.traineddata");

#[derive(Debug, Clone, PartialEq, Ord, Eq, PartialOrd)]
pub enum DirOrFile {
    File(PathBuf),
    Dir(PathBuf),
}

pub type FileMap = BTreeMap<DirOrFile, Vec<u8>>;

#[derive(Debug, Clone)]
pub struct TesseractVm {
    tesseract_compiled_module: Vec<u8>,
}

impl TesseractVm {
    pub fn new() -> Result<Self, String> {
        
        let store = Store::default();
        let mut module = Module::from_binary(&store, &TESSERACT_WASM).unwrap();
        module.set_name("tesseract");
        let bytes = module.serialize().unwrap();
        
        Ok(Self {
            tesseract_compiled_module: bytes.to_vec(),
        })
    }

    /// Returns the .hocr string or an error
    pub fn ocr_image(&self, image_data: &[u8]) -> Result<String, String> {

        let mut store = Store::default();
        let mut module = unsafe { Module::deserialize(
                &store, 
                self.tesseract_compiled_module.clone()
            ) 
        }.map_err(|e| format!("failed to deserialize module: {e}"))?;

        let mut tesseract_files = FileMap::default();
        tesseract_files.insert(
            DirOrFile::File(Path::new("image.png").to_path_buf()), 
            image_data.to_vec(),
        );
        tesseract_files.insert(
            DirOrFile::File(Path::new("eng.traineddata").to_path_buf()), 
            TRAINED_DATA.to_vec(),
        );

        module.set_name("tesseract");
        
        let stdout_pipe = 
            WasiBidirectionalSharedPipePair::new()
            .with_blocking(false);
    
        println!("module ok!");

        let wasi_env = prepare_webc_env(
            &mut store, 
            stdout_pipe.clone(),
            &tesseract_files, 
            "tesseract", 
            &[
                format!("image.png"),
                format!("output"),
                format!("--psm"),
                format!("6"),
                format!("-l"),
                format!("deu"),
                format!("--dpi"),
                format!("300"),
                format!("-c"),
                format!("tessedit_char_whitelist=abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZüÜäÄöÖß,.-/%§()€0123456789 "),
                format!("-c"),
                format!("tessedit_create_hocr=1"),
            ]
        ).map_err(|e| format!("{e}"))?;

        println!("wasi env ok!");

        exec_module(&mut store, &module, wasi_env)
        .map_err(|e| format!("exec_module: {e}"))?;

        Ok(format!("worked!"))
    }
}

fn prepare_webc_env(
    store: &mut Store,
    stdout: WasiBidirectionalSharedPipePair,
    files: &FileMap,
    command: &str,
    args: &[String],
) -> Result<WasiFunctionEnv, String> {
    let fs = MemFileSystem::default();
    for key in files.keys() {
        match key {
            DirOrFile::Dir(d) => { 
                let mut s = format!("{}", d.display());
                if s.is_empty() { continue; }
                let s = format!("/{s}");
                let _ = fs.create_dir(Path::new(&s)); 
            },
            DirOrFile::File(f) => {

            },
        }
    }
    for (k, v) in files.iter() {
        match k {
            DirOrFile::Dir(d) => { continue; },
            DirOrFile::File(d) => { 
                let mut s = format!("{}", d.display());
                if s.is_empty() { continue; }
                let s = format!("/{s}");
                let mut file = fs
                    .new_open_options()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .create(true)
                    .open(&Path::new(&s))
                    .unwrap();
                
                file.write(&v).unwrap();
            },
        }
    }

    let mut wasi_env = WasiState::new(command);
    wasi_env.set_fs(Box::new(fs));

    for key in files.keys() {
        let mut s = match key {
            DirOrFile::Dir(d) => format!("{}", d.display()),
            DirOrFile::File(f) => continue,
        };
        if s.is_empty() { continue; }
        let s = format!("/{s}");
        wasi_env.preopen(|p| {
            p.directory(&s).read(true).write(true).create(true)
        })
        .map_err(|e| format!("E4: {e}"))?;
    }

    for a in args {
        wasi_env.arg(a);
    }

    let wasi_env = wasi_env
    .stdout(Box::new(stdout));

    Ok(
        wasi_env
        .finalize(store)
        .map_err(|e| format!("E5: {e}"))?    
    )
}

fn exec_module(
    store: &mut Store,
    module: &Module,
    mut wasi_env: wasmer_wasi::WasiFunctionEnv,
) -> Result<(), String> {

    let tesseract_imports = tesseract_exports(store, &wasi_env.env);
    let mut import_object = Imports::new();
    for (m, e) in tesseract_imports.into_iter() {
        import_object.define("a", &m, e);
    }

    let instance = Instance::new(store, &module, &import_object)
        .map_err(|e| format!("instance: {e}"))?;
    let memory = instance.exports.get_memory("memory")
        .map_err(|e| format!("memory: {e}"))?;
    wasi_env.data_mut(store).set_memory(memory.clone());

    // If this module exports an _initialize function, run that first.
    if let Ok(initialize) = instance.exports.get_function("_initialize") {
        initialize
            .call(store, &[])
            .map_err(|e| format!("failed to run _initialize function: {e}"))?;
    }

    let result = instance.exports
        .get_function("_start")
        .map_err(|e| format!("_start: {e}"))?
        .call(store, &[])
        .map_err(|e| format!("call: {e}"))?;

    Ok(())
}

fn tesseract_exports(mut store: &mut impl AsStoreMut, env: &FunctionEnv<WasiEnv>) -> Exports {

    /*
        "a": ___cxa_throw,
        "b": ___cxa_allocate_exception,
        "c": __embind_register_class_function,
        "d": __embind_register_memory_view,
        "e": __embind_register_integer,
        "f": __embind_register_value_object_field,
        "g": _abort,
        "h": _setTempRet0,
        "i": __emscripten_date_now,
        "j": __embind_finalize_value_object,
        "k": __embind_register_class_constructor,
        "l": __embind_register_value_object,
        "m": __embind_register_class,
        "n": __emval_take_value,
        "o": _fd_close,
        "p": __embind_register_std_wstring,
        "q": __emval_incref,
        "r": _fd_write,
        "s": ___syscall_fcntl64,
        "t": ___syscall_openat,
        "u": __embind_register_std_string,
        "v": __embind_register_float,
        "w": __embind_register_enum_value,
        "x": _fd_seek,
        "y": __embind_register_bigint,
        "z": _strftime_l

        "A": _emscripten_resize_heap,
        "B": ___syscall_rmdir,
        "C": ___syscall_unlinkat,
        "D": _environ_get,
        "E": __embind_register_enum,
        "F": _environ_sizes_get,
        "G": ___syscall_getcwd,
        "H": _fd_read,
        "I": ___syscall_ioctl,
        "J": _emscripten_get_now,
        "K": __emscripten_get_now_is_monotonic,
        "L": __gmtime_js,
        "M": __localtime_js,
        "N": __mktime_js,
        "O": __tzset_js,
        "P": _emscripten_memcpy_big,
        "Q": __embind_register_emval,
        "R": __embind_register_bool,
        "S": __embind_register_void,
        "T": _strftime,
        "U": __emval_decref,
        "V": __emval_call,
    };
    */
    let namespace = namespace! {
        "a" => Function::new_typed_with_env(&mut store, env, ___cxa_throw::<Memory32>),
        
        "b" => Function::new_typed_with_env(&mut store, env, __embind_register_memory_view::<Memory32>),
        "c" => Function::new_typed_with_env(&mut store, env, __embind_register_value_object_field::<Memory32>),
        "d" => Function::new_typed_with_env(&mut store, env, __embind_register_integer::<Memory32>),
        "e" => Function::new_typed_with_env(&mut store, env, __embind_register_class_function::<Memory32>),
        "f" => Function::new_typed_with_env(&mut store, env, ___cxa_allocate_exception::<Memory32>),
        "g" => Function::new_typed_with_env(&mut store, env, __embind_finalize_value_object::<Memory32>),
        "h" => Function::new_typed_with_env(&mut store, env, _abort::<Memory32>),
        "i" => Function::new_typed_with_env(&mut store, env, __embind_register_value_object::<Memory32>),
        "j" => Function::new_typed_with_env(&mut store, env, _setTempRet0::<Memory32>),
        "k" => Function::new_typed_with_env(&mut store, env, __emscripten_date_now::<Memory32>),
        "l" => Function::new_typed_with_env(&mut store, env, __embind_register_class_constructor::<Memory32>),
        "m" => Function::new_typed_with_env(&mut store, env, __embind_register_class::<Memory32>),
        "n" => Function::new_typed_with_env(&mut store, env, __emval_take_value::<Memory32>),
        "o" => Function::new_typed_with_env(&mut store, env, _fd_close::<Memory32>),
        "p" => Function::new_typed_with_env(&mut store, env, __embind_register_std_wstring::<Memory32>),
        "q" => Function::new_typed_with_env(&mut store, env, __emval_incref::<Memory32>),
        "r" => Function::new_typed_with_env(&mut store, env, _fd_write::<Memory32>),
        "s" => Function::new_typed_with_env(&mut store, env, ___syscall_fcntl64::<Memory32>),
        "t" => Function::new_typed_with_env(&mut store, env, ___syscall_openat::<Memory32>),
        "u" => Function::new_typed_with_env(&mut store, env, __embind_register_std_string::<Memory32>),
        "v" => Function::new_typed_with_env(&mut store, env, __embind_register_float::<Memory32>),
        "w" => Function::new_typed_with_env(&mut store, env, __embind_register_enum_value::<Memory32>),
        "x" => Function::new_typed_with_env(&mut store, env, _fd_seek::<Memory32>),
        "y" => Function::new_typed_with_env(&mut store, env, __embind_register_bigint::<Memory32>),
        "z" => Function::new_typed_with_env(&mut store, env, _strftime_::<Memory32>),

        "A" => Function::new_typed_with_env(&mut store, env, _emscripten_resize_heap::<Memory32>),
        "B" => Function::new_typed_with_env(&mut store, env, ___syscall_rmdir::<Memory32>),
        "C" => Function::new_typed_with_env(&mut store, env, ___syscall_unlinkat::<Memory32>),
        "D" => Function::new_typed_with_env(&mut store, env, _environ_get::<Memory32>),
        "E" => Function::new_typed_with_env(&mut store, env, __embind_register_enum::<Memory32>),
        "F" => Function::new_typed_with_env(&mut store, env, _environ_sizes_get::<Memory32>),
        "G" => Function::new_typed_with_env(&mut store, env, ___syscall_getcwd::<Memory32>),
        "H" => Function::new_typed_with_env(&mut store, env, _fd_read::<Memory32>),
        "I" => Function::new_typed_with_env(&mut store, env, ___syscall_ioctl::<Memory32>),
        "J" => Function::new_typed_with_env(&mut store, env, _emscripten_get_now::<Memory32>),
        "K" => Function::new_typed_with_env(&mut store, env, __emscripten_get_now_is_monotonic::<Memory32>),
        "L" => Function::new_typed_with_env(&mut store, env, __gmtime_js::<Memory32>),
        "M" => Function::new_typed_with_env(&mut store, env, __localtime_js::<Memory32>),
        "N" => Function::new_typed_with_env(&mut store, env, __mktime_js::<Memory32>),
        "O" => Function::new_typed_with_env(&mut store, env, __tzset_js::<Memory32>),
        "P" => Function::new_typed_with_env(&mut store, env, _emscripten_memcpy_big::<Memory32>),
        "Q" => Function::new_typed_with_env(&mut store, env, __embind_register_emval::<Memory32>),
        "R" => Function::new_typed_with_env(&mut store, env, __embind_register_bool::<Memory32>),
        "S" => Function::new_typed_with_env(&mut store, env, __embind_register_void::<Memory32>),
        "T" => Function::new_typed_with_env(&mut store, env, _strftime::<Memory32>),
        "U" => Function::new_typed_with_env(&mut store, env, __emval_decref::<Memory32>),
        "V" => Function::new_typed_with_env(&mut store, env, __emval_call::<Memory32>),
        
        // special _memory function: allocates memory
        "W" => Function::new_typed_with_env(&mut store, env, _memory::<Memory32>),
    };
    namespace
}

pub fn _memory<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn ___cxa_allocate_exception<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn ___cxa_throw<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn ___syscall_fcntl64<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn ___syscall_getcwd<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn ___syscall_ioctl<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn ___syscall_openat<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn ___syscall_rmdir<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn ___syscall_unlinkat<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __embind_finalize_value_object<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __embind_register_bigint<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __embind_register_bool<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __embind_register_class<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __embind_register_class_constructor<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __embind_register_class_function<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __embind_register_emval<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __embind_register_enum<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __embind_register_enum_value<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __embind_register_float<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __embind_register_integer<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __embind_register_memory_view<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __embind_register_std_string<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __embind_register_std_wstring<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __embind_register_value_object<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __embind_register_value_object_field<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __embind_register_void<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __emscripten_date_now<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __emscripten_get_now_is_monotonic<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __emval_call<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __emval_decref<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __emval_incref<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __emval_take_value<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __gmtime_js<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __localtime_js<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __mktime_js<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn __tzset_js<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn _abort<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn _emscripten_get_now<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn _emscripten_memcpy_big<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn _emscripten_resize_heap<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn _environ_get<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn _environ_sizes_get<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn _fd_close<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn _fd_read<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn _fd_seek<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn _fd_write<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn _setTempRet0<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn _strftime<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

pub fn _strftime_<M: MemorySize>(
    mut ctx: FunctionEnvMut<'_, WasiEnv>,
    // argv: WasmPtr<u8, M>,
) -> Errno {
    Errno::Access    
}

fn main() {
    let vm = TesseractVm::new().unwrap();
    println!("ok! compiled to {} bytes", vm.tesseract_compiled_module.len());
    println!("{:?}", vm.ocr_image(include_bytes!("../testocr.png")));
}
