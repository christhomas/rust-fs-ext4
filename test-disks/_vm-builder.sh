#!/bin/sh
# GUEST-side: runs inside the qemu Alpine VM. Expects the caller
# (local.d wrapper) to have already: mounted /host via 9p, and
# extracted attr + acl .apk contents in-place. All of e2fsprogs,
# busybox mount/dd/truncate, setfattr/setfacl are therefore on PATH.
#
# Each build_* function produces ONE ext4-*.img in /host so it lands
# directly on the host filesystem. The mkfs/mount/setfattr/setfacl
# commands are verbatim-compatible with the Linux pipeline that
# previously ran under docker — test expectations baked into the
# capi_* test suite depend on the exact content written here.

set -eu
cd /host

mkdir -p /mnt/img

# --- image builders -------------------------------------------------------

build_basic() {
    local img=ext4-basic.img
    echo "[vm] $img"
    rm -f $img
    truncate -s 16M $img
    mkfs.ext4 -q -F -b 4096 -O has_journal,ext_attr,dir_index,filetype,extent,64bit,flex_bg,sparse_super,metadata_csum \
        -L testvolume $img
    mount -t ext4 -o loop $img /mnt/img
    printf 'hello from ext4\n' > /mnt/img/test.txt
    mkdir -p /mnt/img/subdir
    # /subdir needs at least one entry so rmdir-on-nonempty-dir
    # tests actually hit ENOTEMPTY. Without this, /subdir has only
    # `.` and `..` and the rmdir would succeed.
    echo 'nested' > /mnt/img/subdir/nested.txt
    ln -s test.txt /mnt/img/link.txt
    sync
    umount /mnt/img
}

build_htree() {
    local img=ext4-htree.img
    echo "[vm] $img"
    rm -f $img
    truncate -s 16M $img
    mkfs.ext4 -q -F -b 4096 -O has_journal,ext_attr,dir_index,filetype,extent,64bit,flex_bg,sparse_super,large_file,huge_file,uninit_bg,metadata_csum \
        -L htree-vol $img
    mount -t ext4 -o loop $img /mnt/img
    mkdir -p /mnt/img/bigdir
    i=1
    while [ $i -le 256 ]; do
        printf 'content of file %03d\n' $i > /mnt/img/bigdir/file_$i.txt
        i=$((i + 1))
    done
    echo 'small file content' > /mnt/img/small.txt
    sync
    umount /mnt/img
}

build_csum_seed() {
    local img=ext4-csum-seed.img
    echo "[vm] $img"
    rm -f $img
    truncate -s 16M $img
    mkfs.ext4 -q -F -b 4096 -O has_journal,extent,64bit,flex_bg,metadata_csum,metadata_csum_seed \
        -L csum-seed-vol $img
    mount -t ext4 -o loop $img /mnt/img
    echo 'pi-style file' > /mnt/img/hello.txt
    mkdir -p /mnt/img/etc
    echo 'fake fstab' > /mnt/img/etc/fstab
    sync
    umount /mnt/img
}

build_no_csum() {
    local img=ext4-no-csum.img
    echo "[vm] $img"
    rm -f $img
    truncate -s 8M $img
    mkfs.ext4 -q -F -b 4096 -O ^metadata_csum,extent,64bit,filetype,dir_index,sparse_super \
        -L no-csum-vol $img
    mount -t ext4 -o loop $img /mnt/img
    echo 'no checksum here' > /mnt/img/file.txt
    sync
    umount /mnt/img
}

build_deep_extents() {
    local img=ext4-deep-extents.img
    echo "[vm] $img"
    rm -f $img
    truncate -s 64M $img
    mkfs.ext4 -q -F -b 4096 -O extent,64bit,flex_bg,metadata_csum -L deep-vol $img
    mount -t ext4 -o loop $img /mnt/img
    # Sparse file with 1-byte 'X' writes every 64 KB up to 16 MB —
    # ~245 extents force multi-level extent tree.
    dd if=/dev/zero of=/mnt/img/sparse.bin bs=1 count=0 seek=16M status=none
    off=0
    while [ $off -lt 16000000 ]; do
        printf 'X' | dd of=/mnt/img/sparse.bin bs=1 count=1 seek=$off conv=notrunc status=none
        off=$((off + 65536))
    done
    echo 'control file' > /mnt/img/dense.txt
    sync
    umount /mnt/img
}

build_inline() {
    local img=ext4-inline.img
    echo "[vm] $img"
    rm -f $img
    truncate -s 8M $img
    mkfs.ext4 -q -F -b 4096 -I 256 -O ext_attr,extent,64bit,filetype,dir_index,metadata_csum,inline_data \
        -L inline-vol $img
    mount -t ext4 -o loop $img /mnt/img
    echo 'tiny inline' > /mnt/img/tiny.txt
    printf 'A%.0s' $(seq 1 100) > /mnt/img/medium.txt
    ln -s 'target/path/here' /mnt/img/symlink
    sync
    umount /mnt/img
}

build_xattr() {
    local img=ext4-xattr.img
    echo "[vm] $img"
    rm -f $img
    truncate -s 8M $img
    mkfs.ext4 -q -F -b 4096 -O ext_attr,extent,64bit,filetype,dir_index,metadata_csum,inline_data \
        -L xattr-vol $img
    mount -t ext4 -o loop $img /mnt/img
    echo 'has xattrs' > /mnt/img/tagged.txt
    setfattr -n user.color -v 'red' /mnt/img/tagged.txt
    setfattr -n user.com.apple.FinderInfo -v '0xDEADBEEF' /mnt/img/tagged.txt
    mkdir /mnt/img/tagged_dir
    setfattr -n user.purpose -v 'documents' /mnt/img/tagged_dir
    echo 'no xattrs here' > /mnt/img/plain.txt
    sync
    umount /mnt/img
}

build_acl() {
    local img=ext4-acl.img
    echo "[vm] $img"
    rm -f $img
    truncate -s 8M $img
    mkfs.ext4 -q -F -b 4096 -O ext_attr,extent,64bit,filetype,dir_index,metadata_csum -L acl-vol $img
    tune2fs -o acl,user_xattr $img >/dev/null
    mount -t ext4 -o loop,acl,user_xattr $img /mnt/img
    echo 'minimal acl' > /mnt/img/mode_only.txt
    setfacl -m u::rwx,g::r-x,o::r-- /mnt/img/mode_only.txt
    echo 'named entries' > /mnt/img/named.txt
    setfacl -m u:1000:rw-,g:2000:r--,m::rwx /mnt/img/named.txt
    mkdir /mnt/img/acl_dir
    setfacl -m u::rwx,g::r-x,o::--x,d:u::rwx,d:g::r-x,d:o::--- /mnt/img/acl_dir
    echo 'no acl' > /mnt/img/plain.txt
    sync
    umount /mnt/img
}

build_largedir() {
    local img=ext4-largedir.img
    echo "[vm] $img"
    rm -f $img
    truncate -s 192M $img
    mkfs.ext4 -q -F -b 4096 -N 80000 \
        -O has_journal,ext_attr,dir_index,filetype,extent,64bit,flex_bg,sparse_super,large_file,huge_file,uninit_bg,metadata_csum,large_dir \
        -L largedir-vol $img
    mount -t ext4 -o loop $img /mnt/img
    mkdir -p /mnt/img/huge
    seq -w 1 70000 | while read -r i; do
        : > /mnt/img/huge/file_$i.txt
    done
    echo 'control' > /mnt/img/small.txt
    sync
    umount /mnt/img
}

build_manyfiles() {
    local img=ext4-manyfiles.img
    echo "[vm] $img"
    rm -f $img
    truncate -s 16M $img
    mkfs.ext4 -q -F -b 4096 -O has_journal,ext_attr,dir_index,filetype,extent,64bit,flex_bg,sparse_super,metadata_csum \
        -L many-vol $img
    mount -t ext4 -o loop $img /mnt/img
    i=1
    while [ $i -le 512 ]; do
        printf 'f%04d\n' $i > /mnt/img/file_$i.txt
        i=$((i + 1))
    done
    sync
    umount /mnt/img
}

# --- dispatch -------------------------------------------------------------

ALL="basic htree csum_seed no_csum deep_extents inline xattr acl largedir manyfiles"
TARGETS="${*:-$ALL}"

for t in $TARGETS; do
    case "$t" in
        basic)        build_basic ;;
        htree)        build_htree ;;
        csum_seed)    build_csum_seed ;;
        no_csum)      build_no_csum ;;
        deep_extents) build_deep_extents ;;
        inline)       build_inline ;;
        xattr)        build_xattr ;;
        acl)          build_acl ;;
        largedir)     build_largedir ;;
        manyfiles)    build_manyfiles ;;
        *)            echo "[vm] unknown target: $t (have: $ALL)" >&2; exit 1 ;;
    esac
done

echo "[vm] done — syncing."
sync
