use flate2::read::GzEncoder;
use std::io::Read;

pub struct TarballBuilder {
    prefix: String,
    inner: tar::Builder<Vec<u8>>,
}

impl TarballBuilder {
    pub fn new(name: &str, version: &str) -> Self {
        let prefix = format!("{name}-{version}");
        let inner = tar::Builder::new(vec![]);
        Self { prefix, inner }
    }

    pub fn add_raw_manifest(self, content: &[u8]) -> Self {
        let path = format!("{}/Cargo.toml", self.prefix);
        self.add_file(&path, content)
    }

    pub fn add_file(mut self, path: &str, content: &[u8]) -> Self {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_cksum();
        self.inner.append_data(&mut header, path, content).unwrap();

        self
    }

    pub fn build_unzipped(self) -> Vec<u8> {
        self.inner.into_inner().unwrap()
    }

    pub fn build(self) -> Vec<u8> {
        let tarball_bytes = self.build_unzipped();

        let mut gzip_bytes = vec![];
        GzEncoder::new(tarball_bytes.as_slice(), Default::default())
            .read_to_end(&mut gzip_bytes)
            .unwrap();

        gzip_bytes
    }
}
