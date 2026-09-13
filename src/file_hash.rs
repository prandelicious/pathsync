use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;

const HASH_BUFFER_SIZE: usize = 1024 * 1024;

pub fn xxh3_128(path: &Path) -> io::Result<u128> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = xxhash_rust::xxh3::Xxh3::new();
    let mut buffer = [0_u8; HASH_BUFFER_SIZE];

    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(hasher.digest128())
}
