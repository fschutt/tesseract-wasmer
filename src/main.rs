
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
    let memory = instance.exports.get_memory("X")
        .map_err(|e| format!("memory: {e}"))?;
    
    /*
        var env = {
            "USER": "web_user",
            "LOGNAME": "web_user",
            "PATH": "/",
            "PWD": "/",
            "HOME": "/home/web_user",
            "LANG": lang,
            "_": getExecutableName()
        };
    */

    // init_emval()
    // createWasm()
    wasi_env.data_mut(store).set_memory(memory.clone());

    /*
        Module["___wasm_call_ctors"] = function() {
            return (Module["___wasm_call_ctors"] = Module["asm"]["X"]).apply(null, arguments)
        };
        var _malloc = Module["_malloc"] = function() {
            return (_malloc = Module["_malloc"] = Module["asm"]["Y"]).apply(null, arguments)
        };
        var _free = Module["_free"] = function() {
            return (_free = Module["_free"] = Module["asm"]["_"]).apply(null, arguments)
        };
        var ___getTypeName = Module["___getTypeName"] = function() {
            return (___getTypeName = Module["___getTypeName"] = Module["asm"]["$"]).apply(null, arguments)
        };
        Module["___embind_register_native_and_builtin_types"] = function() {
            return (Module["___embind_register_native_and_builtin_types"] = Module["asm"]["aa"]).apply(null, arguments)
        };
        var ___cxa_is_pointer_type = Module["___cxa_is_pointer_type"] = function() {
            return (___cxa_is_pointer_type = Module["___cxa_is_pointer_type"] = Module["asm"]["ba"]).apply(null, arguments)
        };
        Module["dynCall_jiji"] = function() {
            return (Module["dynCall_jiji"] = Module["asm"]["ca"]).apply(null, arguments)
        };
        Module["dynCall_viijii"] = function() {
            return (Module["dynCall_viijii"] = Module["asm"]["da"]).apply(null, arguments)
        };
        Module["dynCall_iiiiij"] = function() {
            return (Module["dynCall_iiiiij"] = Module["asm"]["ea"]).apply(null, arguments)
        };
        Module["dynCall_iiiiijj"] = function() {
            return (Module["dynCall_iiiiijj"] = Module["asm"]["fa"]).apply(null, arguments)
        };
        Module["dynCall_iiiiiijj"] = function() {
            return (Module["dynCall_iiiiiijj"] = Module["asm"]["ga"]).apply(null, arguments)
        };
        Module["dynCall_jijii"] = function() {
            return (Module["dynCall_jijii"] = Module["asm"]["ha"]).apply(null, arguments)
        };
        Module["dynCall_vijii"] = function() {
            return (Module["dynCall_vijii"] = Module["asm"]["ia"]).apply(null, arguments)
        };
        Module["dynCall_jij"] = function() {
            return (Module["dynCall_jij"] = Module["asm"]["ja"]).apply(null, arguments)
        };
        Module["dynCall_iij"] = function() {
            return (Module["dynCall_iij"] = Module["asm"]["ka"]).apply(null, arguments)
        };
        Module["dynCall_viji"] = function() {
            return (Module["dynCall_viji"] = Module["asm"]["la"]).apply(null, arguments)
        };
        Module["dynCall_jii"] = function() {
            return (Module["dynCall_jii"] = Module["asm"]["ma"]).apply(null, arguments)
        };
    */

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
        "a" => Function::new_typed_with_env(&mut store, env, __embind_register_class_function::<Memory32>),        
        "b" => Function::new_typed_with_env(&mut store, env, __embind_register_memory_view::<Memory32>),
        "c" => Function::new_typed_with_env(&mut store, env, __embind_register_value_object_field::<Memory32>),
        "d" => Function::new_typed_with_env(&mut store, env, __embind_register_integer::<Memory32>),
        "e" => Function::new_typed_with_env(&mut store, env, ___cxa_throw::<Memory32>),
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

// ----------
// 
// "a"."a": [] -> [I32]
pub fn __embind_register_class_function<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
) -> i32 {
    panic!("a.a: __embind_register_class_function")
}

// ----------
// 
//     "a"."b": [I32] -> []
pub fn __embind_register_memory_view<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv: WasmPtr<u8, M>,
) {
    panic!("a.b: __embind_register_memory_view")
}

// ----------
// 
// "c": [I32, I32, I32, I32] -> []
pub fn __embind_register_value_object_field<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _arg1: WasmPtr<u8, M>,
    _arg2: WasmPtr<u8, M>,
    _arg3: WasmPtr<u8, M>,
    _arg4: WasmPtr<u8, M>,
) {
    panic!("a.c: __embind_register_value_object_field");
}

// ----------
// 
// [I32, I32, I32, I32] -> [I32]
pub fn __embind_register_integer<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _arg1: WasmPtr<u8, M>,
    _arg2: WasmPtr<u8, M>,
    _arg3: WasmPtr<u8, M>,
    _arg4: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.d: __embind_register_integer")
}

// ----------
// 
// [I32, I32] -> [I32]
pub fn ___cxa_throw<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _arg1: WasmPtr<u8, M>,
    _arg2: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.e: ___cxa_throw")
}

// ----------
//
// [I32, I32, I32] -> []
pub fn ___cxa_allocate_exception<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _arg1: WasmPtr<u8, M>,
    _arg2: WasmPtr<u8, M>,
    _arg3: WasmPtr<u8, M>,
) {
    panic!("a.f: ___cxa_allocate_exception")
}

// ----------
//
// [I32, I32] -> []
pub fn __embind_finalize_value_object<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _arg1: WasmPtr<u8, M>,
    _arg2: WasmPtr<u8, M>,
) {
    panic!("a.g: __embind_finalize_value_object")
}

// ----------
//
// [I32, I32, I32] -> [I32]
pub fn _abort<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _arg1: WasmPtr<u8, M>,
    _arg2: WasmPtr<u8, M>,
    _arg3: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.h: _abort")
}

// ------
//
// [I32, I32, I32, I32] -> []
pub fn __embind_register_value_object<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _arg1: WasmPtr<u8, M>,
    _arg2: WasmPtr<u8, M>,
    _arg3: WasmPtr<u8, M>,
    _arg4: WasmPtr<u8, M>,
) {
    panic!("a.i: __embind_register_value_object")
}

// -----
// 
// [I32, I32, I32, I32, I32] -> [I32]
pub fn _setTempRet0<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _arg1: WasmPtr<u8, M>,
    _arg2: WasmPtr<u8, M>,
    _arg3: WasmPtr<u8, M>,
    _arg4: WasmPtr<u8, M>,
    _argv5: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.j: _setTempRet0")
}

// -----
// 
// [I32, I32, I32] -> []
pub fn __emscripten_date_now<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _arg1: WasmPtr<u8, M>,
    _arg2: WasmPtr<u8, M>,
    _arg3: WasmPtr<u8, M>,
) {
    panic!("a.k: _setTempRet0")
}

// -----
//
// [I32] -> [I32]
pub fn __embind_register_class_constructor<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _arg1: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.l: __embind_register_class_constructor")
}

// ------
// 
// [I32, I32, I32, I32, I32, I32] -> [I32]
pub fn __embind_register_class<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _arg1: WasmPtr<u8, M>,
    _arg2: WasmPtr<u8, M>,
    _arg3: WasmPtr<u8, M>,
    _arg4: WasmPtr<u8, M>,
    _arg5: WasmPtr<u8, M>,
    _arg6: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.m: __embind_register_class")
}

// -----
// 
// [I32, I32, I32, I32, I32] -> []
pub fn __emval_take_value<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
    _argv3: WasmPtr<u8, M>,
    _argv4: WasmPtr<u8, M>,
    _argv5: WasmPtr<u8, M>,
) {
    panic!("a.n: __emval_take_value")
}

// -----
//
// [I32] -> []
pub fn _fd_close<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv: WasmPtr<u8, M>,
) {
    panic!("a.o: _fd_close")
}

// [] -> []
pub fn __embind_register_std_wstring<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
) {
    panic!("a.p: __embind_register_std_wstring") 
}

// [] -> [F64]
pub fn __emval_incref<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
) -> f64 {
    panic!("a.q: __emval_incref")
}

// [I32, I32, I32, I32] -> [I32]
pub fn _fd_write<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
    _argv3: WasmPtr<u8, M>,
    _argv4: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.r: _fd_write")
}

// ------
//
// [I32] -> [I32]
pub fn ___syscall_fcntl64<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.s: ___syscall_fcntl64")
}

// [I32, I32, I32] -> [I32]
pub fn ___syscall_openat<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
    _argv3: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.t: ___syscall_openat")
}

// [I32, I32, I32, I32] -> [I32]
pub fn __embind_register_std_string<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
    _argv3: WasmPtr<u8, M>,
    _argv4: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.u: __embind_register_std_string")
}

// [I32, I32, I32, I32] -> [I32]
pub fn __embind_register_float<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
    _argv3: WasmPtr<u8, M>,
    _argv4: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.v: __embind_register_float")
}

// [I32, I32, I32] -> [I32]
pub fn __embind_register_enum_value<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
    _argv3: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.w: __embind_register_enum_value")
}

// [I32, I32, I32, I32, I32, I32] -> []
pub fn _fd_seek<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
    _argv3: WasmPtr<u8, M>,
    _argv4: WasmPtr<u8, M>,
    _argv5: WasmPtr<u8, M>,
    _argv6: WasmPtr<u8, M>,
) {
    panic!("a.x: _fd_seek")
}

// [I32, I32, I32, I32, I32, I32, I32, I32, I32, I32] -> []
pub fn __embind_register_bigint<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
    _argv3: WasmPtr<u8, M>,
    _argv4: WasmPtr<u8, M>,
    _argv5: WasmPtr<u8, M>,
    _argv6: WasmPtr<u8, M>,
    _argv7: WasmPtr<u8, M>,
    _argv8: WasmPtr<u8, M>,
    _argv9: WasmPtr<u8, M>,
    _argv10: WasmPtr<u8, M>,
) {
    panic!("a.y: __embind_register_bigint")
}

// [I32, I32, I32, I32, I32] -> [I32]
pub fn _strftime_<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
    _argv3: WasmPtr<u8, M>,
    _argv4: WasmPtr<u8, M>,
    _argv5: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.z: _strftime_")
}

// [I32, I32, I32, I32, I32] -> [I32]
pub fn _emscripten_resize_heap<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
    _argv3: WasmPtr<u8, M>,
    _argv4: WasmPtr<u8, M>,
    _argv5: WasmPtr<u8, M>,
) -> Errno {
    panic!("a.A: _emscripten_resize_heap")
}

// [] -> []
pub fn ___syscall_rmdir<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
) {
    panic!("a.B: ___syscall_rmdir")
}

// [I32] -> [I32]
pub fn ___syscall_unlinkat<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.C: ___syscall_unlinkat")
}

// [I32] -> [I32]
pub fn _environ_get<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.D: _environ_get")
}

// [I32, I32, I32] -> [I32]
pub fn __embind_register_enum<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
    _argv3: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.E: __embind_register_enum")
}

// [I32, I32, I32, I32, I32, I32] -> [I32]
pub fn _environ_sizes_get<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
    _argv3: WasmPtr<u8, M>,
    _argv4: WasmPtr<u8, M>,
    _argv5: WasmPtr<u8, M>,
    _argv6: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.F: _environ_sizes_get")
}

// [I32, I32, I32, I32, I32, I32] -> [I32]
pub fn ___syscall_getcwd<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
    _argv3: WasmPtr<u8, M>,
    _argv4: WasmPtr<u8, M>,
    _argv5: WasmPtr<u8, M>,
    _argv6: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.G: ___syscall_getcwd")
}

// [I32, I32] -> [I32]
pub fn _fd_read<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.H: _fd_read")
}

// [I32, I32] -> [I32]
pub fn ___syscall_ioctl<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.I: ___syscall_ioctl")
}

// [I32, I32] -> [I32]
pub fn _emscripten_get_now<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.J: _emscripten_get_now")
}

// [I32, I32, I32, I32] -> [I32]
pub fn __emscripten_get_now_is_monotonic<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
    _argv3: WasmPtr<u8, M>,
    _argv4: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.K: __emscripten_get_now_is_monotonic")
}

// [I32, I32] -> [I32]
pub fn __gmtime_js<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.L: __gmtime_js")
}

// [I32, I32] -> [I32]
pub fn __localtime_js<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.M: __localtime_js")
}

// [] -> [F64]
pub fn __mktime_js<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
) -> f64 {
    panic!("a.N: __mktime_js")
}

// [] -> [F64]
pub fn __tzset_js<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
) -> i32 {
    panic!("a.O: __tzset_js")
}

// [I32, I32] -> []
pub fn _emscripten_memcpy_big<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
) {
    panic!("a.P: _emscripten_memcpy_big")
}

// [I32, I32] -> []
pub fn __embind_register_emval<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
) {
    panic!("a.Q: __embind_register_emval")
}

// [I32] -> [I32]
pub fn __embind_register_bool<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.R: __embind_register_bool")
}

// ------
// 
// [I32, I32, I32] -> []
pub fn __embind_register_void<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
    _argv3: WasmPtr<u8, M>,
) {
    panic!("a.S: __embind_register_void")
}

// ------
// 
// [I32, I32, I32] -> []
pub fn _strftime<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
    _argv3: WasmPtr<u8, M>,
) {
    panic!("a.T: _strftime")
}

// ------
// 
// [I32, I32, I32] -> [I32]
pub fn __emval_decref<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
    _argv3: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.U: __emval_decref")
}

// [I32] -> []
pub fn __emval_call<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv: WasmPtr<u8, M>,
) {
    panic!("a.V: __emval_call")
}

// [I32, I32, I32, I32] -> [I32]
pub fn _memory<M: MemorySize>(
    mut _ctx: FunctionEnvMut<'_, WasiEnv>,
    _argv1: WasmPtr<u8, M>,
    _argv2: WasmPtr<u8, M>,
    _argv3: WasmPtr<u8, M>,
    _argv4: WasmPtr<u8, M>,
) -> i32 {
    panic!("a.W: _memory")
}

fn main() {
    let vm = TesseractVm::new().unwrap();
    println!("ok! compiled to {} bytes", vm.tesseract_compiled_module.len());
    println!("{:?}", vm.ocr_image(include_bytes!("../testocr.png")));
}
