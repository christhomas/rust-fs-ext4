# Smoke test for the Rust libext4bridge.a — proves the C ABI is compatible
# with the existing ext4bridge/ext4_bridge.h header.
#
# Build:  make
# Run:    make run        (defaults to ../../../test-disks/ext4-basic.img)
# Image:  make run IMAGE=/path/to/your.img

LIB := ../../target/universal/libext4bridge.a
HDR := ../../../ext4bridge/ext4_bridge.h
IMAGE ?= ../../../test-disks/ext4-basic.img

CFLAGS  := -Wall -Wextra -O2 -I../../../ext4bridge
LDFLAGS := -L../../target/universal
LDLIBS  := -lext4bridge -framework Security -framework Foundation -lresolv

smoke: smoke.c $(LIB) $(HDR)
	$(CC) $(CFLAGS) -o $@ smoke.c $(LDFLAGS) $(LDLIBS)

$(LIB):
	cd ../.. && bash build.sh

run: smoke
	./smoke $(IMAGE)

clean:
	rm -f smoke

.PHONY: run clean
