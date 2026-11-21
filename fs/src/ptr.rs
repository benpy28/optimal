//! Block pointers: the 64-byte record every parent keeps about a child.

use crate::codec::{Reader, Writer};
use crate::crypto::{MAC_LEN, NONCE_LEN};
use crate::error::Result;

pub const PTR_LEN: usize = 64;

/// Where a block lives and how to verify it. Because the MAC of a child is
/// stored in its parent, verifying a path from the superblock down proves
/// every block on it is exactly the one that was committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockPtr {
    pub addr: u64,
    /// Generation (commit number) the block was written in.
    pub birth: u64,
    pub mac: [u8; MAC_LEN],
    pub nonce: [u8; NONCE_LEN],
    /// Meaning depends on the pointer's role: tree level for tree nodes,
    /// free-block count for space-map blocks.
    pub aux: u32,
}

impl BlockPtr {
    pub const NULL: BlockPtr = BlockPtr {
        addr: 0,
        birth: 0,
        mac: [0; MAC_LEN],
        nonce: [0; NONCE_LEN],
        aux: 0,
    };

    pub fn is_null(&self) -> bool {
        self.addr == 0
    }

    pub fn encode(&self, w: &mut Writer) {
        w.u64(self.addr);
        w.u64(self.birth);
        w.bytes(&self.mac);
        w.bytes(&self.nonce);
        w.u32(self.aux);
        w.zeros(4);
    }

    pub fn decode(r: &mut Reader) -> Result<BlockPtr> {
        let addr = r.u64()?;
        let birth = r.u64()?;
        let mac = r.array()?;
        let nonce = r.array()?;
        let aux = r.u32()?;
        if r.u32()? != 0 {
            return Err(r.err());
        }
        Ok(BlockPtr {
            addr,
            birth,
            mac,
            nonce,
            aux,
        })
    }
}
