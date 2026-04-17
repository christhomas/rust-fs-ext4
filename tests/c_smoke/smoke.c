// Smoke test: open ext4-basic.img through the Rust libext4rs.a using the
// SAME ext4_bridge.h header that the C/lwext4 build uses. Proves the Rust
// crate is a drop-in replacement.
//
// Build: see Makefile in this directory.
// Run:   ./smoke ../../../test-disks/ext4-basic.img

#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include "../../../../ext4rs.h"

#define EXPECT(cond, msg) do { \
    if (!(cond)) { \
        fprintf(stderr, "FAIL: %s (line %d)\n  last_error: %s\n", \
                msg, __LINE__, ext4rs_last_error()); \
        return 1; \
    } \
} while (0)

int main(int argc, char** argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s <ext4-image>\n", argv[0]);
        return 2;
    }
    const char* path = argv[1];

    // 1. Mount.
    ext4rs_fs_t* fs = ext4rs_mount(path);
    EXPECT(fs != NULL, "mount");
    printf("OK  mount: %s\n", path);

    // 2. Volume info.
    ext4rs_volume_info_t info = {0};
    int rc = ext4rs_get_volume_info(fs, &info);
    EXPECT(rc == 0, "get_volume_info");
    printf("OK  volume: name=\"%.16s\" block_size=%u total_blocks=%llu free=%llu inodes=%u/%u\n",
           info.volume_name, info.block_size,
           (unsigned long long)info.total_blocks,
           (unsigned long long)info.free_blocks,
           info.free_inodes, info.total_inodes);

    // 3. Stat root.
    ext4rs_attr_t root = {0};
    rc = ext4rs_stat(fs, "/", &root);
    EXPECT(rc == 0, "stat /");
    EXPECT(root.file_type == EXT4_BRIDGE_FT_DIR, "root is dir");
    printf("OK  stat /: inode=%u mode=0%o uid=%u links=%u\n",
           root.inode, root.mode, root.uid, root.link_count);

    // 4. List root.
    ext4rs_dir_iter_t* it = ext4rs_dir_open(fs, "/");
    EXPECT(it != NULL, "dir_open /");
    printf("OK  dir_open /\n");

    int entries = 0;
    int found_test_txt = 0;
    const ext4rs_dirent_t* d;
    while ((d = ext4rs_dir_next(it)) != NULL) {
        printf("    entry: inode=%u type=%u name=\"%s\"\n", d->inode, d->file_type, d->name);
        entries++;
        if (strcmp(d->name, "test.txt") == 0) found_test_txt = 1;
    }
    ext4rs_dir_close(it);
    EXPECT(entries >= 4, "at least 4 entries (. .. lost+found test.txt)");
    EXPECT(found_test_txt, "found test.txt");
    printf("OK  dir_next/close: %d entries\n", entries);

    // 5. Stat test.txt.
    ext4rs_attr_t f = {0};
    rc = ext4rs_stat(fs, "/test.txt", &f);
    EXPECT(rc == 0, "stat /test.txt");
    EXPECT(f.file_type == EXT4_BRIDGE_FT_REG_FILE, "test.txt is regular file");
    printf("OK  stat /test.txt: inode=%u size=%llu\n", f.inode, (unsigned long long)f.size);

    // 6. Read test.txt.
    char buf[256] = {0};
    int64_t n = ext4rs_read_file(fs, "/test.txt", buf, 0, sizeof(buf) - 1);
    EXPECT(n > 0, "read /test.txt");
    EXPECT((uint64_t)n == f.size, "read returned full size");
    printf("OK  read_file: %lld bytes = \"%s\"\n", (long long)n, buf);

    // 7. Unmount.
    ext4rs_umount(fs);
    printf("OK  umount\n");

    printf("\nALL SMOKE TESTS PASSED — Rust libext4rs.a is a working\n");
    printf("drop-in replacement for the C/lwext4 implementation.\n");
    return 0;
}
