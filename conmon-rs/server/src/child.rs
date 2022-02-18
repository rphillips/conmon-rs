#[derive(Debug)]
pub struct Child {
    pub id: String,
    pub pid: i32,
    pub exit_paths: Vec<std::path::PathBuf>,
    pub bundle_path: String,
}

impl Child {
    pub fn new(
        id: String,
        bundle_path: String,
        pid: i32,
        exit_paths: Vec<std::path::PathBuf>,
    ) -> Self {
        Self {
            id,
            bundle_path,
            pid,
            exit_paths,
        }
    }
}
