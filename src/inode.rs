use fuse::{FileType, FileAttr};
use sequence_trie::SequenceTrie;
use std::collections::HashMap;
use std::ops::{Index, IndexMut};
use time;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use super::Metadata;

#[derive(Debug, Clone)]
pub struct Inode {
    pub path: PathBuf,
    pub attr: FileAttr,
    pub visited: bool,
}

impl Inode {
    pub fn new<P: AsRef<Path>>(path: P, attr: FileAttr) -> Inode {
        Inode {
            path: PathBuf::from(path.as_ref()),
            attr: attr,
            visited: false,
        }
    }
}

pub struct InodeStore {
    inode_map: HashMap<u64, Inode>,
    ino_trie: SequenceTrie<OsString, u64>,
    uid: u32,
    gid: u32,
}

impl InodeStore {
    pub fn new(perm: u16, uid: u32, gid: u32) -> InodeStore {
        let mut store = InodeStore {
            inode_map: HashMap::new(),
            ino_trie: SequenceTrie::new(),
            uid: uid,
            gid: gid,
        };

        let now = time::now_utc().to_timespec();
        let fs_root = FileAttr {
            ino: 1,
            size: 0,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind: FileType::Directory,
            perm: perm,
            nlink: 0,
            uid: uid,
            gid: gid,
            rdev: 0,
            flags: 0,
        };

        store.insert(Inode::new("/", fs_root));
        store
    }

    pub fn len(&self) -> usize {
        self.inode_map.len()
    }

    pub fn get(&self, ino: u64) -> Option<&Inode> {
        self.inode_map.get(&ino)
    }

    pub fn insert_metadata<P: AsRef<Path>>(&mut self, path: P, metadata: &Metadata) -> &Inode {
        let ino = self.len() as u64 + 1;
        println!("insert metadata: {} {}", ino, path.as_ref().display());

        let attr = FileAttr {
            ino: ino,
            size: metadata.size,
            blocks: 0,
            atime: metadata.atime,
            mtime: metadata.mtime,
            ctime: metadata.ctime,
            crtime: metadata.crtime,
            kind: metadata.kind,
            perm: metadata.perm,
            nlink: 0,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            flags: 0,
        };

        // TODO: stop using to_string_lossy, and make the inode trie built from OsStr components
        self.insert(Inode::new(path, attr));
        self.get(ino).unwrap()
    }

    pub fn child(&self, ino: u64, name: &Path) -> Option<&Inode> {
        self.get(ino)
            .and_then(|inode| {
                let mut sequence = path_to_sequence(&inode.path);
                sequence.push(name.as_os_str().into());
                self.ino_trie.get(&sequence).and_then(|ino| self.get(*ino) )
            })
    }

    pub fn children(&self, ino: u64) -> Vec<&Inode> {
        match self.get(ino) {
            Some(inode) => {
                let sequence = path_to_sequence(&inode.path);
                let node = self.ino_trie.get_node(&sequence)
                    .expect("inconsistent fs - failed to lookup by path after lookup by ino");
                node.children
                    .values()
                    .filter_map(|ref c| c.value.as_ref() )
                    .map(|ino| self.get(*ino).expect("inconsistent fs - found child without inode") )
                    .collect()
            }
            None => vec![],
        }
    }

    // All inodes have a parent (root parent is root)
    // Return value of None means the ino wasn't found
    pub fn parent(&self, ino: u64) -> Option<&Inode> {
        // parent of root is root
        if ino == 1 {
            return self.get(1);
        }

        self.get(ino)
            .and_then(|inode| {
                let sequence = path_to_sequence(&inode.path);
                match sequence.len() {
                    1 => self.get(1),
                    len => self.ino_trie.get(&sequence[0..(len-1)]).and_then(|p_ino| self.get(*p_ino) )
                }
            })
    }

    pub fn get_mut(&mut self, ino: u64) -> Option<&mut Inode> {
        self.inode_map.get_mut(&ino)
    }

    // Only insert new node. Panics if there is an ino collision in the map or trie
    pub fn insert(&mut self, inode: Inode) {
        let ino = inode.attr.ino;
        let sequence = path_to_sequence(&inode.path);

        if self.inode_map.insert(ino, inode).is_some() {
            panic!("Corrupt inode store: reinserted ino {} into inode_map", ino);
        }

        if !self.ino_trie.insert(&sequence, ino) {
            let mut node = self.ino_trie.get_mut_node(&sequence)
                                .expect(&format!("Corrupt inode store: couldn't insert or modify ino_trie at {:?}", &sequence));
            // TODO: figure out why this check triggers a false alarm panic on backspacing to dir and then tabbing
            // if node.value.is_some() {
            //     panic!("Corrupt inode store: reinserted ino {} into ino_trie, prev value: {}", ino, node.value.unwrap());
            // }
            node.value = Some(ino);
        }
    }

    pub fn remove(&mut self, ino: u64) {
        let sequence = {
            let ref path = self.inode_map[&ino].path;
            path_to_sequence(&path)
        };

        self.inode_map.remove(&ino);
        self.ino_trie.remove(&sequence);

        assert!(self.inode_map.get(&ino).is_none());
        assert!(self.ino_trie.get(&sequence).is_none());
    }
}

impl Index<u64> for InodeStore {
    type Output = Inode;

    fn index<'a>(&'a self, index: u64) -> &'a Inode {
        self.get(index).unwrap()
    }
}

impl IndexMut<u64> for InodeStore {
    fn index_mut<'a>(&'a mut self, index: u64) -> &'a mut Inode {
        self.get_mut(index).unwrap()
    }
}

fn path_to_sequence(path: &Path) -> Vec<OsString> {
    path.iter().map(|s| s.to_owned() ).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use time;
    use std::path::Path;
    use fuse::{FileType, FileAttr};

    fn new_dir_attr(ino: u64) -> FileAttr {
        let now = time::now_utc().to_timespec();
        FileAttr {
            ino: ino,
            size: 0,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind: FileType::Directory,
            perm: 0o750,
            nlink: 2,
            uid: 1000,
            gid: 1000,
            rdev: 0,
            flags: 0,
        }
    }

    fn new_file_attr(ino: u64) -> FileAttr {
        let now = time::now_utc().to_timespec();
        FileAttr {
            ino: ino,
            size: 42,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            crtime: now,
            kind: FileType::Directory,
            perm: 0o640,
            nlink: 2,
            uid: 1000,
            gid: 1000,
            rdev: 0,
            flags: 0,
        }
    }

    fn build_basic_store() -> InodeStore {
        let mut store = InodeStore::new(0o750, 1000, 1000);
        store.insert(Inode::new("/data", new_dir_attr(2)));
        store.insert(Inode::new("/data/foo.txt", new_file_attr(3)));
        store.insert(Inode::new("/data/bar.txt", new_file_attr(4)));
        store
    }

    #[test]
    fn test_inode_store_get() {
        let store = build_basic_store();
        assert_eq!(&store.get(1).unwrap().path, Path::new("/"));
        assert_eq!(&store.get(2).unwrap().path, Path::new("/data"));
        assert_eq!(&store.get(3).unwrap().path, Path::new("/data/foo.txt"));
    }

    #[test]
    fn test_inode_store_get_mut() {
        let mut store = build_basic_store();
        {
            let mut inode = store.get_mut(3).unwrap();
            assert_eq!(inode.attr.size, 42);
            inode.attr.size = 23;
        }
        assert_eq!(store.get(3).unwrap().attr.size, 23);
    }


    #[test]
    fn test_inode_store_parent() {
        let store = build_basic_store();
        assert_eq!(&store.parent(3).unwrap().path, Path::new("/data"));
        assert_eq!(store.parent(2).unwrap().attr.ino, 1);
        assert_eq!(store.parent(1).unwrap().attr.ino, 1);
        assert!(&store.parent(999).is_none());
    }

    #[test]
    fn test_inode_store_children() {
        let store = build_basic_store();
        assert_eq!(store.children(1).len(), 1);
        assert_eq!(store.children(2).len(), 2);
        assert_eq!(store.children(3).len(), 0);
    }

    #[test]
    fn test_inode_store_child() {
        let store = build_basic_store();
        assert_eq!(store.child(2, Path::new("foo.txt")).unwrap().path, Path::new("/data/foo.txt"));
        assert!(store.child(2, Path::new("notfound")).is_none());
    }

    #[test]
    fn test_inode_store_insert_backward() {
        let mut store = InodeStore::new(0o750, 1000, 1000);
        store.insert(Inode::new("/data/foo/bar.txt", new_file_attr(4)));
        store.insert(Inode::new("/data/foo", new_dir_attr(3)));
        store.insert(Inode::new("/data", new_dir_attr(2)));

        // lookup by ino
        assert_eq!(&store.get(1).unwrap().path, Path::new("/"));
        assert_eq!(&store.get(2).unwrap().path, Path::new("/data"));
        assert_eq!(&store.get(3).unwrap().path, Path::new("/data/foo"));
        assert_eq!(&store.get(4).unwrap().path, Path::new("/data/foo/bar.txt"));
    }

}