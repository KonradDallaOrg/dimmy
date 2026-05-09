import Foundation

// MARK: - WavPeaks
//
// Mac port of Win `Helpers/WavPeaks.cs`. Reads a WAV file, walks the
// `fmt ` + `data` chunks, decodes samples (PCM int8/16/24/32 + IEEE
// float32), and bucketises the maximum absolute amplitude into
// `bucketCount` normalized peaks in [0, 1]. Used by the meeting Done
// view to draw a click-to-seek waveform.
//
// Hermetic: no FFI, no dispatch, no allocations beyond the output
// buffer + a 16 KiB read buffer for the data chunk. Returns an empty
// array on any parse failure — the UI falls back to a thin slider.

enum WavPeaks {
    /// Read `path` and produce `bucketCount` normalized peaks. Returns
    /// `[]` on any error (missing file, unknown format, truncated WAV).
    static func readPeaks(path: String, bucketCount: Int) -> [Float] {
        guard bucketCount > 0,
              FileManager.default.fileExists(atPath: path),
              let data = try? Data(contentsOf: URL(fileURLWithPath: path),
                                    options: [.alwaysMapped])
        else { return [] }
        return parse(data: data, bucketCount: bucketCount)
    }

    static func parse(data: Data, bucketCount: Int) -> [Float] {
        // Need at least the RIFF header + fmt + data headers.
        guard data.count >= 44 else { return [] }
        guard read4cc(data, at: 0) == "RIFF",
              read4cc(data, at: 8) == "WAVE"
        else { return [] }

        var fmtTag: UInt16 = 0
        var channels: UInt16 = 1
        var bitsPerSample: UInt16 = 0
        var dataOffset: Int = -1
        var dataSize: Int = 0

        var pos = 12
        while pos + 8 <= data.count {
            let id = read4cc(data, at: pos)
            let size = Int(readU32LE(data, at: pos + 4))
            let bodyStart = pos + 8
            switch id {
            case "fmt ":
                guard bodyStart + 16 <= data.count else { return [] }
                fmtTag = readU16LE(data, at: bodyStart)
                channels = readU16LE(data, at: bodyStart + 2)
                // sampleRate at +4, byteRate at +8, blockAlign at +12
                bitsPerSample = readU16LE(data, at: bodyStart + 14)
            case "data":
                dataOffset = bodyStart
                dataSize = size
            default:
                break
            }
            if dataOffset >= 0 { break }
            pos = bodyStart + size
            // WAV chunk bodies are padded to even length.
            if size % 2 == 1 { pos += 1 }
        }

        guard dataOffset >= 0,
              channels > 0,
              bitsPerSample > 0,
              bitsPerSample % 8 == 0
        else { return [] }

        let bytesPerSample = Int(bitsPerSample / 8)
        let frameSize = bytesPerSample * Int(channels)
        guard frameSize > 0 else { return [] }
        let totalFrames = min(dataSize, data.count - dataOffset) / frameSize
        guard totalFrames > 0 else { return [] }

        var peaks = [Float](repeating: 0, count: bucketCount)
        let framesPerBucket = max(1, totalFrames / bucketCount)

        for b in 0..<bucketCount {
            let startFrame = b * framesPerBucket
            let endFrame = min(totalFrames, (b + 1) * framesPerBucket)
            if startFrame >= endFrame { break }
            var peak: Float = 0
            for f in startFrame..<endFrame {
                let frameOffset = dataOffset + f * frameSize
                var maxCh: Float = 0
                for c in 0..<Int(channels) {
                    let s = readSample(
                        data: data,
                        offset: frameOffset + c * bytesPerSample,
                        fmtTag: fmtTag,
                        bits: Int(bitsPerSample)
                    )
                    let a = abs(s)
                    if a > maxCh { maxCh = a }
                }
                if maxCh > peak { peak = maxCh }
            }
            peaks[b] = min(peak, 1)
        }
        return peaks
    }

    // MARK: - Decoders

    private static func readSample(data: Data, offset: Int, fmtTag: UInt16, bits: Int) -> Float {
        // 1=PCM int, 3=IEEE float (per WAVE_FORMAT_PCM / IEEE_FLOAT).
        if fmtTag == 3, bits == 32 {
            return readF32LE(data, at: offset)
        }
        if bits == 16 {
            let v = readI16LE(data, at: offset)
            return Float(v) / 32_768.0
        }
        if bits == 8 {
            // 8-bit WAV is unsigned with center 128.
            return (Float(data[offset]) - 128.0) / 128.0
        }
        if bits == 24 {
            let b0 = Int32(data[offset])
            let b1 = Int32(data[offset + 1])
            let b2 = Int32(Int8(bitPattern: data[offset + 2]))  // sign-extend
            let v = (b2 << 16) | (b1 << 8) | b0
            return Float(v) / 8_388_608.0
        }
        if bits == 32, fmtTag == 1 {
            let v = readI32LE(data, at: offset)
            return Float(v) / 2_147_483_648.0
        }
        return 0
    }

    // MARK: - Little-endian readers

    private static func read4cc(_ data: Data, at offset: Int) -> String {
        guard offset + 4 <= data.count else { return "" }
        let bytes = data[offset..<(offset + 4)]
        return String(bytes: bytes, encoding: .ascii) ?? ""
    }

    private static func readU16LE(_ data: Data, at offset: Int) -> UInt16 {
        UInt16(data[offset]) | (UInt16(data[offset + 1]) << 8)
    }

    private static func readU32LE(_ data: Data, at offset: Int) -> UInt32 {
        UInt32(data[offset])
            | (UInt32(data[offset + 1]) << 8)
            | (UInt32(data[offset + 2]) << 16)
            | (UInt32(data[offset + 3]) << 24)
    }

    private static func readI16LE(_ data: Data, at offset: Int) -> Int16 {
        Int16(bitPattern: readU16LE(data, at: offset))
    }

    private static func readI32LE(_ data: Data, at offset: Int) -> Int32 {
        Int32(bitPattern: readU32LE(data, at: offset))
    }

    private static func readF32LE(_ data: Data, at offset: Int) -> Float {
        Float(bitPattern: readU32LE(data, at: offset))
    }
}
