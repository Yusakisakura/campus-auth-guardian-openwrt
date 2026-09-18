#!/usr/bin/env python3
"""Verify ipk structure."""
import sys, gzip, io

path = sys.argv[1] if len(sys.argv) > 1 else "dist/campus-auth-guardian_0.2.0-1_aarch64_cortex-a53.ipk"

with open(path, "rb") as f:
    data = f.read()

print(f"ipk size: {len(data)} bytes")
assert data[:8] == b"!<arch>\n", "bad magic"
print("magic: OK")

pos = 8
members = []
for i in range(3):
    hdr = data[pos:pos+60]
    assert hdr[58:60] == b"`\n", f"member {i}: bad header terminator"
    name = hdr[:16].decode("ascii").strip()
    size = int(hdr[48:58].strip())
    assert b"\x00" not in hdr, f"member {i}: NUL in header!"
    members.append((name, size, pos + 60))
    pos += 60 + size
    if size % 2:
        pos += 1

print(f"members: {[m[0] for m in members]}")
print(f"computed end: {pos}, file size: {len(data)}, match: {pos == len(data)}")

for name, size, start in members:
    chunk = data[start:start+size]
    print(f"\n--- {name} ({size} bytes) ---")
    if chunk[:2] == b"\x1f\x8b":
        print("  gzip header: OK")
        try:
            decompressed = gzip.decompress(chunk)
            print(f"  decompressed: {len(decompressed)} bytes")
            if name.endswith(".tar.gz"):
                import tarfile
                tf = tarfile.open(fileobj=io.BytesIO(decompressed), mode="r:")
                members_list = tf.getmembers()
                print(f"  tar entries: {len(members_list)}")
                for m in members_list[:5]:
                    print(f"    {oct(m.mode)} {m.name}")
                if len(members_list) > 5:
                    print(f"    ... and {len(members_list)-5} more")
                tf.close()
        except Exception as e:
            print(f"  ERROR: {e}")
    elif name == "debian-binary":
        print(f"  content: {repr(chunk)}")
    else:
        print(f"  first 20 bytes: {chunk[:20].hex()}")
