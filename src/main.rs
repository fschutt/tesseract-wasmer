
use wasmer::{Store, Module, Instance};
use wasmer_wasi::{WasiFunctionEnv, WasiBidirectionalSharedPipePair, WasiState};
use wasmer_vfs::{FileSystem, mem_fs::FileSystem as MemFileSystem};
use std::path::{Path, PathBuf};
use std::collections::BTreeMap;

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

        exec_module(&mut store, &module, wasi_env)
        .map_err(|e| format!("{e}"))?;

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

    let import_object = wasi_env.import_object(store, &module)
        .map_err(|e| format!("{e}"))?;
    let instance = Instance::new(store, &module, &import_object)
        .map_err(|e| format!("{e}"))?;
    let memory = instance.exports.get_memory("memory")
        .map_err(|e| format!("{e}"))?;
    wasi_env.data_mut(store).set_memory(memory.clone());

    // If this module exports an _initialize function, run that first.
    if let Ok(initialize) = instance.exports.get_function("_initialize") {
        initialize
            .call(store, &[])
            .map_err(|e| format!("failed to run _initialize function: {e}"))?;
    }

    let result = instance.exports
        .get_function("_start")
        .map_err(|e| format!("{e}"))?
        .call(store, &[])
        .map_err(|e| format!("{e}"))?;

    Ok(())
}

fn main() {
    let vm = TesseractVm::new().unwrap();
    println!("{:?}", vm.ocr_image(include_bytes!("../testocr.png")));
}
