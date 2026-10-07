export function syntheticWav() {
    const count = 16000;
    const wav = Buffer.alloc(44 + count * 2);
    wav.write('RIFF');
    wav.writeUInt32LE(wav.length - 8, 4);
    wav.write('WAVEfmt ', 8);
    wav.writeUInt32LE(16, 16);
    wav.writeUInt16LE(1, 20);
    wav.writeUInt16LE(1, 22);
    wav.writeUInt32LE(16000, 24);
    wav.writeUInt32LE(32000, 28);
    wav.writeUInt16LE(2, 32);
    wav.writeUInt16LE(16, 34);
    wav.write('data', 36);
    wav.writeUInt32LE(count * 2, 40);
    for (let i = 0; i < count; i++)
        wav.writeInt16LE(Math.round(Math.sin(i * 2 * Math.PI * 440 / 16000) * 1000), 44 + i * 2);
    return wav;
}
