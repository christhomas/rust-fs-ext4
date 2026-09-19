#!/usr/bin/env bash
# Rebuild fuzz/corpus from filesystems mke2fs wrote.
#
# ext4 is the widest parser surface in the family: a superblock, group
# descriptors, inodes, directory blocks, extent trees, htree indexes and
# a jbd2 journal, each read from an offset the one before it supplied.
#
# The seeds are real filesystems and real structures cut out of them. A
# random byte string is refused by the 0xEF53 magic on the first line
# and never reaches the arithmetic underneath.
#
# Usage: scripts/make-fuzz-corpus.sh
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/ext4-fuzz-corpus.XXXXXX")"
trap 'rm -rf "$work"' EXIT

command -v mke2fs >/dev/null || {
    echo "mke2fs not found; install e2fsprogs" >&2
    exit 1
}

# Populated through mke2fs's -d, not by mounting: mounting needs root
# and -d does not, and it still produces trees a real e2fsprogs wrote.
tree="$work/tree"
mkdir -p "$tree/sub"
head -c 120000 /dev/urandom > "$tree/random.bin"
python3 -c "import sys; open(sys.argv[1],'w').write('the quick brown fox. ' * 4000)" "$tree/text.txt"
echo "deep" > "$tree/sub/deep.txt"
ln -sf sub/deep.txt "$tree/link"
# Enough entries that the root directory indexes with an htree rather
# than staying a linear list, which is a different decoder.
for i in $(seq 1 600); do
    : > "$tree/entry-$(printf '%04d' "$i")"
done
command -v setfattr >/dev/null && {
    setfattr -n user.colour -v blue "$tree/text.txt"
    setfattr -n user.long -v "$(printf 'v%.0s' $(seq 1 100))" "$tree/text.txt"
} || true

rm -rf "$here/fuzz/corpus"
mkdir -p "$here/fuzz/corpus"/{image,superblock,inode,dir_block,journal}

build() {
    local name="$1" size="$2"; shift 2
    local img="$here/fuzz/corpus/image/$name.img"
    truncate -s "$size" "$img"
    mke2fs -q -F -d "$tree" "$@" "$img" 2>/dev/null || {
        echo "mke2fs could not build the '$name' filesystem" >&2
        exit 1
    }
}

# One per shape that changes a decoder rather than the layout: ext2 has
# no extents and no journal, ext4 has both, 1 KiB blocks move every
# offset, and 64bit widens the group descriptors.
build ext2      4M  -t ext2 -b 1024
build ext4      8M  -t ext4 -b 1024 -O ^64bit
build ext4-4k   8M  -t ext4 -b 4096 -O ^64bit
build ext4-64bit 8M  -t ext4 -b 4096 -O 64bit,metadata_csum

python3 - "$here/fuzz/corpus" <<'PY'
import os, struct, sys

root = sys.argv[1]
SB_AT = 1024
SB_LEN = 1024
EXT_MAGIC = 0xEF53
JBD2_MAGIC = b'\xc0\x3b\x39\x98'

def write(kind, name, data):
    with open(os.path.join(root, kind, name), 'wb') as f:
        f.write(data)

inodes = dirs = journals = 0
for img_name in sorted(os.listdir(os.path.join(root, 'image'))):
    stem = img_name[:-len('.img')]
    img = open(os.path.join(root, 'image', img_name), 'rb').read()

    sb = img[SB_AT:SB_AT + SB_LEN]
    magic, = struct.unpack_from('<H', sb, 56)
    assert magic == EXT_MAGIC, f"{img_name}: superblock magic is {magic:#x}"
    write('superblock', f'{stem}.bin', sb)

    log_block_size, = struct.unpack_from('<I', sb, 24)
    blocksize = 1024 << log_block_size
    first_data_block, = struct.unpack_from('<I', sb, 20)
    inode_size, = struct.unpack_from('<H', sb, 88)
    feature_incompat, = struct.unpack_from('<I', sb, 96)
    sixty_four = bool(feature_incompat & 0x80)
    desc_size, = struct.unpack_from('<H', sb, 254)
    if not sixty_four or desc_size == 0:
        desc_size = 32

    # The group descriptor table begins in the block after the one
    # holding the superblock.
    gdt_at = (first_data_block + 1) * blocksize
    inode_table_lo, = struct.unpack_from('<I', img, gdt_at + 8)
    inode_table = inode_table_lo * blocksize

    # Inode 2 is the root; inodes are one-based, so it is the second
    # slot in the table. 256 bytes rather than `inode_size` so the
    # inline xattr area behind a large inode comes with it.
    root_inode_at = inode_table + inode_size
    write('inode', f'{stem}-root.bin', img[root_inode_at:root_inode_at + max(inode_size, 256)])
    inodes += 1

    # A directory block, found by its own shape: the first entry of any
    # ext4 directory block is "." -- inode number, a record length, a
    # name length of 1 and the name itself.
    for at in range(0, len(img) - blocksize + 1, blocksize):
        block = img[at:at + blocksize]
        ino, rec_len = struct.unpack_from('<IH', block, 0)
        name_len = block[6]
        if ino == 0 or rec_len < 12 or rec_len > blocksize or name_len != 1:
            continue
        if block[8:9] != b'.':
            continue
        write('dir_block', f'{stem}-at{at // blocksize}.bin', block)
        dirs += 1
        break

    # The journal superblock, found by the jbd2 magic. ext2 has no
    # journal, which is part of why it is in the corpus.
    for at in range(0, len(img) - blocksize + 1, blocksize):
        if img[at:at + 4] == JBD2_MAGIC:
            write('journal', f'{stem}-at{at // blocksize}.bin', img[at:at + blocksize])
            journals += 1
            break

assert inodes and dirs, "no inode or directory block found -- the layout walk needs revisiting"
assert journals, "no journal superblock found -- did every filesystem come out without one?"
print(f"{inodes} inodes, {dirs} directory blocks, {journals} journals")
PY

echo "corpus rebuilt under fuzz/corpus:"
find "$here/fuzz/corpus" -type f | sort | sed "s#$here/##"
echo "total: $(find "$here/fuzz/corpus" -type f | wc -l) seeds, $(du -sh "$here/fuzz/corpus" | cut -f1)"
