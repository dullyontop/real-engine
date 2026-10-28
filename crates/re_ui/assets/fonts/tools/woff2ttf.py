"""Converts a WOFF 1.0 file to a TrueType/OpenType (sfnt) file."""
import struct, sys, zlib

def convert(src, dst):
    data = open(src, 'rb').read()
    sig, flavor, length, num, _r, _total, _maj, _min, _mo, _ml, _mol, _po, _pl = struct.unpack('>4sIIHHIHHIIIII', data[:44])
    assert sig == b'wOFF', src
    entries = []
    for i in range(num):
        tag, off, clen, olen, cksum = struct.unpack('>4sIIII', data[44 + 20 * i: 64 + 20 * i])
        raw = data[off:off + clen]
        table = zlib.decompress(raw) if clen < olen else raw
        assert len(table) == olen
        entries.append((tag, cksum, table))
    entries.sort(key=lambda e: e[0])
    es = 1
    while es * 2 <= num: es *= 2
    entry_sel = es.bit_length() - 1
    search = es * 16
    out = bytearray(struct.pack('>IHHHH', flavor, num, search, entry_sel, num * 16 - search))
    offset = 12 + 16 * num
    body = bytearray()
    dirs = bytearray()
    for tag, cksum, table in entries:
        dirs += struct.pack('>4sIII', tag, cksum, offset + len(body), len(table))
        body += table + b'\0' * ((4 - len(table) % 4) % 4)
    open(dst, 'wb').write(bytes(out + dirs + body))

if __name__ == '__main__':
    convert(sys.argv[1], sys.argv[2])
