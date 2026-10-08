"""Regression probes for buffered compression output and ZIP CRC integrity."""
import io
import zipfile
import zlib
import bz2
import lzma

payload = b"buffered output\n" * 70000 + b"tail!"


def zlib_empty_input():
    decoder = zlib.decompressobj()
    assert decoder.decompress(b"") == b""
    assert not decoder.eof, "empty input fabricated end-of-stream"


def zlib_roundtrip():
    for window in (15, -15, 31):
        encoded = zlib.compress(payload, wbits=window)
        assert zlib.decompress(encoded, window) == payload
        decoder = zlib.decompressobj(window)
        result = []
        incoming = encoded
        for _ in range(len(payload) // 65536 + 4):
            result.append(decoder.decompress(incoming, 65536))
            incoming = decoder.unconsumed_tail
            if decoder.eof:
                break
        result.append(decoder.flush())
        assert decoder.eof
        assert b"".join(result) == payload, (window, len(b"".join(result)))
    decoder = zlib.decompressobj()
    encoded = zlib.compress(payload)
    head = decoder.decompress(encoded, 1)
    assert head + decoder.flush() == payload
    assert decoder.eof


def zip_crc():
    archive = io.BytesIO()
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as writer:
        writer.writestr("payload.cube", payload)
    with zipfile.ZipFile(archive) as reader:
        with reader.open("payload.cube") as member:
            output = []
            while True:
                chunk = member.read(65536)
                if not chunk:
                    break
                output.append(chunk)
        assert b"".join(output) == payload


def buffered_codecs():
    for module, factory in ((bz2, bz2.BZ2Decompressor), (lzma, lzma.LZMADecompressor)):
        decoder = factory()
        assert decoder.decompress(b"") == b""
        assert not decoder.eof
        decoder = factory()
        encoded = module.compress(payload)
        output = []
        incoming = encoded
        for _ in range(len(payload) // 65536 + 4):
            chunk = decoder.decompress(incoming, max_length=65536)
            assert len(chunk) <= 65536
            output.append(chunk)
            if decoder.eof:
                break
            assert not decoder.needs_input, "buffered output must remain drainable"
            incoming = b""
        assert decoder.eof
        assert b"".join(output) == payload


def dictionary_and_truncation():
    dictionary = b"buffered output\n"
    encoder = zlib.compressobj(zdict=dictionary)
    encoded = encoder.compress(payload) + encoder.flush()
    decoder = zlib.decompressobj(zdict=dictionary)
    assert decoder.decompress(encoded) + decoder.flush() == payload
    assert decoder.eof
    decoder = zlib.decompressobj()
    decoder.decompress(zlib.compress(payload)[:-4])
    assert not decoder.eof


for probe in (zlib_empty_input, zlib_roundtrip, zip_crc, buffered_codecs, dictionary_and_truncation):
    probe()
print("COMPRESSION_OK")
