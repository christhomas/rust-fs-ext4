/*
 * lwext4-report — a THIRD implementation of ext4, asked what it sees.
 *
 * e2fsprogs and this crate read the same specification and share its
 * ambiguities; the Linux driver is the thing the images are for. lwext4
 * (BSD-2-Clause, github.com/gkostka/lwext4) is none of those: an
 * independent pure-C ext2/3/4 implementation with no code lineage in
 * common with either the kernel or us. Where it disagrees with this
 * crate, one of the two has read the format wrong — which is the whole
 * point of running it.
 *
 * lwext4 ships no tool that lists a filesystem, so this is that tool:
 * it mounts an image through liblwext4's file block device and prints
 *
 *     <kind>\t<path>\t<value>
 *
 * for every path below the root, with `kind` one of type, mode, size,
 * sha256, target. tests/support/src/lwext4.rs parses exactly that, and
 * tests/lwext4_cross_validate.rs compares it against what this crate's
 * own reader says about the same bytes.
 *
 * `write` is the other direction: lwext4 creates a known tree and this
 * crate reads it back. It prints the same report of what it wrote, so
 * the expectation the Rust side compares against is the writer's own
 * account rather than a copy of it kept somewhere else.
 *
 * WHERE THIS RUNS: in the fs-linux-test-harness guest, and nowhere
 * else. scripts/vm-setup.sh builds lwext4 there at a pinned commit;
 * tests/support/src/lwext4.rs compiles this file against it in the same
 * guest. Nothing lwext4-shaped is ever installed on a host.
 *
 * usage: lwext4-report read  <image>
 *        lwext4-report write <image>
 */

#include <ext4.h>
#include <ext4_errno.h>
#include <file_dev.h>

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* The mount point inside lwext4's own namespace. Paths are reported
 * relative to it, so "dir/file" here is "dir/file" to the Rust side. */
#define MP "/mp/"
#define DEV "image"

/* Longer than any path these images hold; `join` refuses to truncate. */
#define PATH_CAP 4096

static void die(const char *what, int code)
{
    fprintf(stderr, "lwext4-report: %s: %d (%s)\n", what, code, strerror(code));
    exit(2);
}

/* ------------------------------------------------------------------ *
 * SHA-256 (FIPS 180-4), spelled out so this program links against
 * liblwext4 and the C library and nothing else.
 * ------------------------------------------------------------------ */

struct sha256 {
    uint32_t state[8];
    uint8_t block[64];
    size_t held;
    uint64_t length;
};

static const uint32_t SHA256_K[64] = {
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1,
    0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
    0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
    0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
    0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
    0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2};

static uint32_t rotr(uint32_t x, int n) { return (x >> n) | (x << (32 - n)); }

static void sha256_compress(struct sha256 *h, const uint8_t *block)
{
    uint32_t w[64], v[8];
    int i;

    for (i = 0; i < 16; i++)
        w[i] = ((uint32_t)block[i * 4] << 24) | ((uint32_t)block[i * 4 + 1] << 16) |
               ((uint32_t)block[i * 4 + 2] << 8) | (uint32_t)block[i * 4 + 3];
    for (i = 16; i < 64; i++) {
        uint32_t s0 = rotr(w[i - 15], 7) ^ rotr(w[i - 15], 18) ^ (w[i - 15] >> 3);
        uint32_t s1 = rotr(w[i - 2], 17) ^ rotr(w[i - 2], 19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16] + s0 + w[i - 7] + s1;
    }
    for (i = 0; i < 8; i++)
        v[i] = h->state[i];
    for (i = 0; i < 64; i++) {
        uint32_t s1 = rotr(v[4], 6) ^ rotr(v[4], 11) ^ rotr(v[4], 25);
        uint32_t ch = (v[4] & v[5]) ^ (~v[4] & v[6]);
        uint32_t t1 = v[7] + s1 + ch + SHA256_K[i] + w[i];
        uint32_t s0 = rotr(v[0], 2) ^ rotr(v[0], 13) ^ rotr(v[0], 22);
        uint32_t maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
        uint32_t t2 = s0 + maj;
        v[7] = v[6]; v[6] = v[5]; v[5] = v[4]; v[4] = v[3] + t1;
        v[3] = v[2]; v[2] = v[1]; v[1] = v[0]; v[0] = t1 + t2;
    }
    for (i = 0; i < 8; i++)
        h->state[i] += v[i];
}

static void sha256_init(struct sha256 *h)
{
    static const uint32_t iv[8] = {0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                                   0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19};
    memcpy(h->state, iv, sizeof iv);
    h->held = 0;
    h->length = 0;
}

static void sha256_update(struct sha256 *h, const uint8_t *data, size_t len)
{
    h->length += len;
    while (len) {
        size_t take = 64 - h->held;
        if (take > len)
            take = len;
        memcpy(h->block + h->held, data, take);
        h->held += take;
        data += take;
        len -= take;
        if (h->held == 64) {
            sha256_compress(h, h->block);
            h->held = 0;
        }
    }
}

static void sha256_hex(struct sha256 *h, char *out)
{
    uint64_t bits = h->length * 8;
    size_t i;

    h->block[h->held++] = 0x80;
    if (h->held > 56) {
        while (h->held < 64)
            h->block[h->held++] = 0;
        sha256_compress(h, h->block);
        h->held = 0;
    }
    while (h->held < 56)
        h->block[h->held++] = 0;
    for (i = 0; i < 8; i++)
        h->block[56 + i] = (uint8_t)(bits >> (56 - 8 * i));
    sha256_compress(h, h->block);
    for (i = 0; i < 8; i++)
        sprintf(out + i * 8, "%08x", h->state[i]);
    out[64] = '\0';
}

/* ------------------------------------------------------------------ *
 * The payload both sides generate: an LCG, so a misplaced block shows
 * as a hash mismatch rather than as plausible-looking zeroes. The Rust
 * side has the same generator (tests/lwext4_cross_validate.rs).
 * ------------------------------------------------------------------ */

static void payload(uint8_t *out, size_t len)
{
    uint64_t state = 0x123456789abcdef0ULL;
    size_t at = 0;

    while (at < len) {
        int i;
        state = state * 6364136223846793005ULL + 1442695040888963407ULL;
        for (i = 0; i < 8 && at < len; i++, at++)
            out[at] = (uint8_t)(state >> (8 * i));
    }
}

/* ------------------------------------------------------------------ *
 * The report
 * ------------------------------------------------------------------ */

static const char *type_name(uint8_t inode_type)
{
    switch (inode_type) {
    case EXT4_DE_REG_FILE: return "regular-file";
    case EXT4_DE_DIR:      return "directory";
    case EXT4_DE_SYMLINK:  return "symbolic-link";
    case EXT4_DE_CHRDEV:   return "character-device";
    case EXT4_DE_BLKDEV:   return "block-device";
    case EXT4_DE_FIFO:     return "fifo";
    case EXT4_DE_SOCK:     return "socket";
    default:               return "unknown";
    }
}

static void emit(const char *kind, const char *path, const char *value)
{
    printf("%s\t%s\t%s\n", kind, path, value);
}

static void emit_mode(const char *guest, const char *rel)
{
    uint32_t mode = 0;
    char text[16];
    int r = ext4_mode_get(guest, &mode);

    if (r != EOK)
        die(guest, r);
    sprintf(text, "%o", mode & 07777);
    emit("mode", rel, text);
}

static void emit_file(const char *guest, const char *rel)
{
    static uint8_t buffer[65536];
    struct sha256 h;
    ext4_file f;
    char text[32];
    char digest[65];
    uint64_t size;
    int r = ext4_fopen(&f, guest, "rb");

    if (r != EOK)
        die(guest, r);
    size = ext4_fsize(&f);
    sprintf(text, "%llu", (unsigned long long)size);
    emit("size", rel, text);

    sha256_init(&h);
    for (;;) {
        size_t got = 0;
        r = ext4_fread(&f, buffer, sizeof buffer, &got);
        if (r != EOK)
            die(guest, r);
        if (!got)
            break;
        sha256_update(&h, buffer, got);
    }
    ext4_fclose(&f);
    sha256_hex(&h, digest);
    emit("sha256", rel, digest);
}

static void emit_symlink(const char *guest, const char *rel)
{
    char target[4096];
    size_t got = 0;
    int r = ext4_readlink(guest, target, sizeof target - 1, &got);

    if (r != EOK)
        die(guest, r);
    target[got] = '\0';
    emit("target", rel, target);
}

/* One directory's entries, taken in full before anything recurses into
 * them: lwext4's directory handle walks the on-disk blocks as it goes,
 * and nothing in its contract says a second open may not disturb it. */
struct names {
    char **name;
    uint8_t *type;
    size_t count;
};

static void names_free(struct names *n)
{
    size_t i;
    for (i = 0; i < n->count; i++)
        free(n->name[i]);
    free(n->name);
    free(n->type);
}

static void names_read(const char *guest_dir, struct names *out)
{
    ext4_dir dir;
    const ext4_direntry *de;
    size_t capacity = 16;
    int r = ext4_dir_open(&dir, guest_dir);

    if (r != EOK)
        die(guest_dir, r);
    out->count = 0;
    out->name = malloc(capacity * sizeof *out->name);
    out->type = malloc(capacity * sizeof *out->type);
    if (!out->name || !out->type)
        die("out of memory", ENOMEM);

    while ((de = ext4_dir_entry_next(&dir)) != NULL) {
        char *copy;
        if (de->name_length == 1 && de->name[0] == '.')
            continue;
        if (de->name_length == 2 && de->name[0] == '.' && de->name[1] == '.')
            continue;
        if (out->count == capacity) {
            capacity *= 2;
            out->name = realloc(out->name, capacity * sizeof *out->name);
            out->type = realloc(out->type, capacity * sizeof *out->type);
            if (!out->name || !out->type)
                die("out of memory", ENOMEM);
        }
        copy = malloc((size_t)de->name_length + 1);
        if (!copy)
            die("out of memory", ENOMEM);
        memcpy(copy, de->name, de->name_length);
        copy[de->name_length] = '\0';
        out->name[out->count] = copy;
        out->type[out->count] = de->inode_type;
        out->count++;
    }
    ext4_dir_close(&dir);
}

/* snprintf that refuses to truncate: a path silently cut short would be
 * reported as a different path, which is the one failure mode a
 * cross-validator must never have. */
static void join(char *out, size_t cap, const char *a, const char *b, const char *c)
{
    int n = snprintf(out, cap, "%s%s%s", a, b, c);

    if (n < 0 || (size_t)n >= cap) {
        fprintf(stderr, "lwext4-report: path too long: %s%s%s\n", a, b, c);
        exit(2);
    }
}

static void walk(const char *guest_dir, const char *rel_dir)
{
    struct names entries;
    size_t i;

    names_read(guest_dir, &entries);
    for (i = 0; i < entries.count; i++) {
        char guest[PATH_CAP];
        char rel[PATH_CAP];

        join(guest, sizeof guest, guest_dir, entries.name[i], "");
        join(rel, sizeof rel, rel_dir, entries.name[i], "");
        emit("type", rel, type_name(entries.type[i]));
        emit_mode(guest, rel);
        switch (entries.type[i]) {
        case EXT4_DE_REG_FILE:
            emit_file(guest, rel);
            break;
        case EXT4_DE_SYMLINK:
            emit_symlink(guest, rel);
            break;
        case EXT4_DE_DIR: {
            char guest_sub[PATH_CAP];
            char rel_sub[PATH_CAP];
            join(guest_sub, sizeof guest_sub, guest_dir, entries.name[i], "/");
            join(rel_sub, sizeof rel_sub, rel_dir, entries.name[i], "/");
            walk(guest_sub, rel_sub);
            break;
        }
        default:
            break;
        }
    }
    names_free(&entries);
}

/* ------------------------------------------------------------------ *
 * The tree lwext4 writes, for the other direction
 * ------------------------------------------------------------------ */

#define BIG_LEN (300 * 1024 + 517)
#define SMALL_LEN 61

static void make_dir(const char *rel, uint32_t mode)
{
    char guest[512];
    char text[16];
    int r;

    snprintf(guest, sizeof guest, MP "%s", rel);
    r = ext4_dir_mk(guest);
    if (r != EOK)
        die(guest, r);
    r = ext4_mode_set(guest, mode | 0040000);
    if (r != EOK)
        die(guest, r);
    emit("type", rel, "directory");
    sprintf(text, "%o", mode);
    emit("mode", rel, text);
}

static void make_file(const char *rel, uint32_t mode, const uint8_t *data, size_t len)
{
    char guest[512];
    char text[32];
    char digest[65];
    struct sha256 h;
    ext4_file f;
    size_t written = 0;
    int r;

    snprintf(guest, sizeof guest, MP "%s", rel);
    r = ext4_fopen(&f, guest, "wb");
    if (r != EOK)
        die(guest, r);
    if (len) {
        r = ext4_fwrite(&f, data, len, &written);
        if (r != EOK)
            die(guest, r);
        if (written != len)
            die(guest, EIO);
    }
    ext4_fclose(&f);
    r = ext4_mode_set(guest, mode | 0100000);
    if (r != EOK)
        die(guest, r);

    emit("type", rel, "regular-file");
    sprintf(text, "%o", mode);
    emit("mode", rel, text);
    sprintf(text, "%zu", len);
    emit("size", rel, text);
    sha256_init(&h);
    sha256_update(&h, data, len);
    sha256_hex(&h, digest);
    emit("sha256", rel, digest);
}

static void make_symlink(const char *rel, const char *target)
{
    char guest[512];
    int r;

    snprintf(guest, sizeof guest, MP "%s", rel);
    r = ext4_fsymlink(target, guest);
    if (r != EOK)
        die(guest, r);
    emit("type", rel, "symbolic-link");
    emit_mode(guest, rel);
    emit("target", rel, target);
}

/* The tree, and the report of it. Deliberately more than one block per
 * directory and more than one extent per file: a reader that mishandles
 * a second directory block or a split extent has to fail here. */
static void write_tree(void)
{
    static uint8_t big[BIG_LEN];
    uint8_t small[SMALL_LEN];
    char name[64];
    int i;

    payload(big, sizeof big);
    payload(small, sizeof small);

    make_dir("dir", 0755);
    make_dir("dir/nested", 0700);
    make_file("dir/small.txt", 0644, small, sizeof small);
    make_file("dir/nested/deep.bin", 0600, big, sizeof big);
    make_file("empty", 0640, big, 0);
    make_symlink("dir/link", "small.txt");
    /* Enough entries to spill the root directory past one block. */
    for (i = 0; i < 40; i++) {
        snprintf(name, sizeof name, "many/f%02d", i);
        if (i == 0)
            make_dir("many", 0755);
        make_file(name, 0644, small, (size_t)i);
    }
}

/* ------------------------------------------------------------------ */

int main(int argc, char **argv)
{
    struct ext4_blockdev *bd;
    const char *mode;
    const char *image;
    int r;

    if (argc != 3) {
        fprintf(stderr, "usage: lwext4-report read|write <image>\n");
        return 2;
    }
    mode = argv[1];
    image = argv[2];

    file_dev_name_set(image);
    bd = file_dev_get();
    if (!bd) {
        fprintf(stderr, "lwext4-report: no block device for %s\n", image);
        return 2;
    }

    r = ext4_device_register(bd, DEV);
    if (r != EOK)
        die("ext4_device_register", r);

    if (strcmp(mode, "read") == 0) {
        r = ext4_mount(DEV, MP, true);
        if (r != EOK)
            die("ext4_mount (read-only)", r);
        walk(MP, "");
    } else if (strcmp(mode, "write") == 0) {
        r = ext4_mount(DEV, MP, false);
        if (r != EOK)
            die("ext4_mount (read-write)", r);
        ext4_cache_write_back(MP, true);
        write_tree();
        ext4_cache_write_back(MP, false);
    } else {
        fprintf(stderr, "lwext4-report: unknown mode %s\n", mode);
        return 2;
    }

    r = ext4_umount(MP);
    if (r != EOK)
        die("ext4_umount", r);
    ext4_device_unregister(DEV);
    return 0;
}
