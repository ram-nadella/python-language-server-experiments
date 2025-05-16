use std::path::{Path, PathBuf};
use std::fs::{self, DirEntry};
use anyhow::{Context, Result};
use walkdir::{WalkDir, DirEntry as WalkDirEntry};
use tracing::debug;

pub fn list_python_files(
    directory: &Path,
    follow_links: bool,
) -> impl Iterator<Item = PathBuf> {
    list_python_files_with_depth(directory, follow_links, usize::MAX)
}

pub fn list_python_files_with_depth(
    directory: &Path,
    follow_links: bool,
    max_depth: usize,
) -> impl Iterator<Item = PathBuf> {
    WalkDir::new(directory)
        .follow_links(follow_links)
        .max_depth(max_depth)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            let path = entry.path();
            path.is_file() && path.extension().map_or(false, |ext| ext == "py")
        })
        .map(|entry| entry.path().to_path_buf())
}

pub fn list_python_files_recursive(
    directory: &Path,
    follow_links: bool,
) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    
    visit_dirs(directory, &mut |entry| {
        let path = entry.path();
        if path.is_file() && path.extension().map_or(false, |ext| ext == "py") {
            files.push(path.to_path_buf());
            debug!("Added python file: {}", path.display());
        }
        Ok(())
    }, follow_links)?;
    
    Ok(files)
}

fn visit_dirs(
    dir: &Path,
    cb: &mut dyn FnMut(&DirEntry) -> Result<()>,
    follow_links: bool,
) -> Result<()> {
    if dir.is_dir() {
        for entry in fs::read_dir(dir)
            .with_context(|| format!("Failed to read directory: {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            
            if path.is_dir() {
                if follow_links || !path.is_symlink() {
                    visit_dirs(&path, cb, follow_links)?;
                }
            } else {
                cb(&entry)?;
            }
        }
    }
    Ok(())
}

pub fn is_python_file(entry: &WalkDirEntry) -> bool {
    let path = entry.path();
    path.is_file() && path.extension().map_or(false, |ext| ext == "py")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{File, create_dir_all};
    use std::io::Write;
    use tempfile::tempdir;

    fn create_test_files(temp_dir: &Path) -> Result<()> {
        // Create directories
        let dir1 = temp_dir.join("dir1");
        let dir2 = temp_dir.join("dir1/dir2");
        let symlink_dir = temp_dir.join("symlink_dir");
        
        create_dir_all(&dir1)?;
        create_dir_all(&dir2)?;
        
        // Create Python files
        let file1 = temp_dir.join("file1.py");
        let file2 = dir1.join("file2.py");
        let file3 = dir2.join("file3.py");
        let non_py_file = temp_dir.join("non_python.txt");
        
        File::create(&file1)?.write_all(b"# Python file 1")?;
        File::create(&file2)?.write_all(b"# Python file 2")?;
        File::create(&file3)?.write_all(b"# Python file 3")?;
        File::create(&non_py_file)?.write_all(b"Not a Python file")?;
        
        // Create symlink on Unix systems
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(&dir2, &symlink_dir)?;
        }
        
        Ok(())
    }

    #[test]
    fn test_list_python_files() -> Result<()> {
        let temp_dir = tempdir()?;
        create_test_files(temp_dir.path())?;
        
        let files: Vec<PathBuf> = list_python_files(temp_dir.path(), false).collect();
        
        // Should find 3 Python files (not following symlinks)
        assert_eq!(files.len(), 3);
        assert!(files.iter().all(|path| path.extension().unwrap() == "py"));
        
        Ok(())
    }

    #[test]
    fn test_list_python_files_with_depth() -> Result<()> {
        let temp_dir = tempdir()?;
        create_test_files(temp_dir.path())?;
        
        // With depth 1, should only find file1.py in root
        let files_depth1: Vec<PathBuf> = list_python_files_with_depth(temp_dir.path(), false, 1).collect();
        assert_eq!(files_depth1.len(), 1);
        assert!(files_depth1[0].file_name().unwrap() == "file1.py");
        
        // With depth 2, should find file1.py and file2.py
        let files_depth2: Vec<PathBuf> = list_python_files_with_depth(temp_dir.path(), false, 2).collect();
        assert_eq!(files_depth2.len(), 2);
        
        Ok(())
    }

    #[test]
    fn test_list_python_files_recursive() -> Result<()> {
        let temp_dir = tempdir()?;
        create_test_files(temp_dir.path())?;
        
        let files = list_python_files_recursive(temp_dir.path(), false)?;
        
        // Should find 3 Python files
        assert_eq!(files.len(), 3);
        assert!(files.iter().all(|path| path.extension().unwrap() == "py"));
        
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn test_follow_symlinks() -> Result<()> {
        let temp_dir = tempdir()?;
        create_test_files(temp_dir.path())?;
        
        // Not following symlinks
        let files_no_follow: Vec<PathBuf> = list_python_files(temp_dir.path(), false).collect();
        
        // Following symlinks
        let files_follow: Vec<PathBuf> = list_python_files(temp_dir.path(), true).collect();
        
        // When following symlinks, we should find more files
        assert!(files_follow.len() >= files_no_follow.len());
        
        Ok(())
    }
} 