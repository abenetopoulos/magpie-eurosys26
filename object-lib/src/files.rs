use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use lazy_static::lazy_static;
use memmap2::{Advice, MmapMut, MmapOptions};
use nando_support::ObjectId;

use crate::error::FileError;

pub static MAX_FILE_SIZE_BYTES: usize = 1_073_741_824;
pub static PRESSURE_THRESHOLD: f64 = 0.93 * MAX_FILE_SIZE_BYTES as f64;

lazy_static! {
    static ref ROOT_ALLOCATION_DIR: PathBuf = {
        let home_path = match std::env::var("ROOT_ALLOCATION_DIR") {
            Err(_) => "/tmp/magpie/".to_string(),
            Ok(p) => p,
        };

        PathBuf::from(home_path)
    };
}

pub fn set_up_allocation_dir() -> Result<(), FileError> {
    let dir = ROOT_ALLOCATION_DIR.to_path_buf();

    match fs::create_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("Failed to create allocation dir: {}", e);
            Err(FileError::DirCreationError(dir))
        }
    }
}

pub fn clear_allocation_dir() {
    let dir = ROOT_ALLOCATION_DIR.to_path_buf();

    match fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("Failed to empty allocation dir: {}", e);
        }
    };
}

pub fn get_object_directory_iterator() -> fs::ReadDir {
    let dir = ROOT_ALLOCATION_DIR.to_path_buf();

    fs::read_dir(&dir).expect("failed to read object directory contents")
}

fn construct_path(file_id: &str) -> PathBuf {
    ROOT_ALLOCATION_DIR.join(Path::new(file_id))
}

pub struct FileHandle {
    pub mapped_file: Arc<RwLock<MmapMut>>,
    pub file_size: usize,
    // FIXME we'll probably end up not needing this now that we're changing how we allocate, so we
    // need to eliminate its usages (especially around fetching file bytes for rsync)
    pub next_write_offset: usize,

    pub allocation_marker: usize,

    pub file_path: PathBuf,

    file: Option<fs::File>,
}

impl FileHandle {
    pub fn can_support_write(&self, write_size_bytes: usize) -> bool {
        self.next_write_offset + write_size_bytes <= MAX_FILE_SIZE_BYTES
    }

    pub fn is_under_pressure(&self) -> bool {
        self.file_size >= PRESSURE_THRESHOLD.floor() as usize
    }

    #[cfg(not(feature = "no-persist"))]
    pub fn as_bytes(&self) -> *const [u8] {
        let file_meta = fs::metadata(&self.file_path).unwrap();
        let mapped_file = self.mapped_file.read().unwrap();
        &(*mapped_file)[0..file_meta.len() as usize]
    }

    #[cfg(feature = "no-persist")]
    pub fn as_bytes(&self) -> *const [u8] {
        let mapped_file = self.mapped_file.read().unwrap();
        let len = self.file_size;

        &(*mapped_file)[0..len]
    }

    pub fn advise(&self) -> () {
        let mapped_file = self.mapped_file.read().unwrap();
        mapped_file
            .advise(Advice::Sequential)
            .expect("Failed to advise");
    }

    pub fn file_len(&self) -> u64 {
        let file_meta = fs::metadata(&self.file_path).unwrap();
        file_meta.len()
    }
}

fn open(path: &PathBuf) -> Result<(fs::File, usize), FileError> {
    let mut file_options = fs::OpenOptions::new();
    file_options
        .read(true)
        .write(true)
        .create(true)
        .append(false)
        .truncate(false);

    match file_options.open(path) {
        Ok(f) => {
            let file_size = f.metadata().unwrap().len() as usize;
            Ok((f, file_size))
        }
        Err(e) => {
            eprintln!("Failed to open {:?}: {e}", path);
            Err(FileError::UnknownError())
        }
    }
}

#[cfg(not(feature = "no-persist"))]
pub fn open_for_id(id: ObjectId, size: usize) -> Result<FileHandle, FileError> {
    let id_as_string = id.to_string();
    let file_path = construct_path(&id_as_string);
    let (file, current_file_size) = match open(&file_path) {
        Ok(t) => t,
        Err(e) => return Err(e),
    };

    let new_file_size = match current_file_size > size {
        false => {
            file.set_len(size as u64)
                .expect("Failed to adjust backing file size");
            size
        }
        true => current_file_size,
    };

    let mapped_file = unsafe {
        match MmapOptions::new().len(MAX_FILE_SIZE_BYTES).map_mut(&file) {
            Ok(m) => RwLock::new(m),
            Err(e) => {
                eprintln!("Failed to allocate: {:?}", e);
                return Err(FileError::UnknownError());
            }
        }
    };

    Ok(FileHandle {
        mapped_file: Arc::new(mapped_file),
        file_size: new_file_size,
        next_write_offset: current_file_size,

        // TODO we actually need to store this somewhere in the file. As it currently stands, this
        // might lead to internal fragmentation, given that the space between the last write
        // from before the object was last dropped and this open might be greater than zero.
        allocation_marker: new_file_size,

        file_path,
        file: Some(file),
    })
}

#[cfg(feature = "no-persist")]
pub fn open_for_id(_id: ObjectId, _size: usize) -> Result<FileHandle, FileError> {
    let mapped_file = match MmapOptions::new().len(MAX_FILE_SIZE_BYTES).map_anon() {
        Ok(m) => RwLock::new(m),
        _ => return Err(FileError::UnknownError()),
    };

    Ok(FileHandle {
        mapped_file: Arc::new(mapped_file),
        file_size: 0,
        next_write_offset: 0,

        // TODO we actually need to store this somewhere in the file. As it currently stands, this
        // might lead to internal fragmentation, given that the space between the last write
        // from before the object was last dropped and this open might be greater than zero.
        allocation_marker: 0,

        // NOTE this is ignored in this case
        file_path: PathBuf::default(),
        file: None,
    })
}

pub fn create_object_copy(src: ObjectId, dst: ObjectId) {
    let src_file_path = construct_path(&src.to_string());
    let dst_file_path = construct_path(&dst.to_string());

    fs::copy(src_file_path, dst_file_path).expect("failed to create copy of object's backing file");
}

pub fn remap(file_handle: &mut FileHandle) {
    let file = match &file_handle.file {
        Some(f) => f,
        None => todo!(),
    };
    let file_size = file.metadata().unwrap().len() as usize;
    let mapped_file = unsafe {
        match MmapMut::map_mut(&*file) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("Failed to re-map file: {}", e);
                panic!("Failed to re-map file");
            }
        }
    };
    file_handle.allocation_marker = file_size;
    let mut mapped_file_ref = file_handle.mapped_file.write().unwrap();
    *mapped_file_ref = mapped_file;
}

pub fn resize(file_handle: &mut FileHandle, new_size: usize) -> Result<(), FileError> {
    let file = match &file_handle.file {
        Some(f) => f,
        None => return Err(FileError::NoMappedFileError(file_handle.file_path.clone())),
    };

    if new_size >= MAX_FILE_SIZE_BYTES {
        return Err(FileError::NoSpaceLeftError());
    }

    file.set_len(new_size as u64)
        .expect("Failed to adjust backing file size");
    file_handle.file_size = new_size;

    Ok(())
}

pub fn write(file_handle: &mut FileHandle, offset: usize, source: &[u8]) -> () {
    let base_idx: usize = offset.try_into().unwrap();

    let source_len = source.len();
    if file_handle.file_size < offset + source_len {
        let starting_size = file_handle.file_size;
        resize(file_handle, starting_size + source_len).expect("failed to resize before write");
        remap(file_handle);
    } else {
        // TODO remove
        println!("no need to resize?");
    }

    let mut file = file_handle.mapped_file.write().unwrap();

    (&mut file[base_idx..(base_idx + source.len())])
        .write_all(source)
        .expect("failed to write region");
}

pub fn sync(file_handle: &FileHandle) -> () {
    file_handle
        .mapped_file
        .read()
        .unwrap()
        .flush()
        .expect("failed to flush");
}

pub fn sync_range(file_handle: &FileHandle, offset: u64, size: u64) -> () {
    file_handle
        .mapped_file
        .read()
        .unwrap()
        .flush_async_range(offset.try_into().unwrap(), size.try_into().unwrap())
        .expect("failed to flush");
}

pub fn write_and_sync(file_handle: &mut FileHandle, offset: usize, source: &[u8]) -> () {
    write(file_handle, offset, source);
    sync(file_handle)
}

pub fn read_from(file_handle: &FileHandle, offset: u64, size: u64) -> *mut [u8] {
    let mut file = file_handle.mapped_file.write().unwrap();
    let base_idx: usize = offset.try_into().unwrap();
    let safe_size: usize = size.try_into().unwrap();

    &mut file[base_idx..(base_idx + safe_size)]
}

pub fn delete_storage(file_handle: &FileHandle) -> Result<(), std::io::Error> {
    fs::remove_file(file_handle.file_path.as_path())
}
