/*
 * Reference implementation for the golden signature test.
 *
 * A line-by-line Java transliteration of ArchiveTune's
 * shazamkit/src/main/kotlin/moe/koiverse/rukamori/shazamkit/ShazamSignatureGenerator.kt
 * (GPL-3.0, © Rukamori and ArchiveTune contributors), kept so the Kotlin algorithm can
 * be run without a Kotlin toolchain:
 *
 *   java ReferenceSignature.java synth_16k_mono.s16le > synth_16k_mono.sig.txt
 *
 * Input: raw 16 kHz mono signed 16-bit little-endian PCM. Output: the signature URI and
 * the sample duration in ms, one per line.
 */

import java.io.ByteArrayOutputStream;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Base64;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.zip.CRC32;

public class ReferenceSignature {
    record Peak(int fftPassNumber, int peakMagnitude, int correctedPeakFrequencyBin) {}

    static final int SAMPLE_RATE = 16000;
    static final double MAX_TIME = 3.1;
    static final int MAX_PEAKS = 255;

    int[] pending = new int[0];
    int processed = 0;
    double[] window = new double[2048];
    double[] real = new double[2048];
    double[] imag = new double[2048];
    int[] ring = new int[2048];
    int ringPos = 0;
    double[][] fftOutputs = new double[256][1025];
    int fftPos = 0;
    double[][] spreadOutputs = new double[256][1025];
    int spreadPos = 0;
    int spreadWritten = 0;
    int signatureNumberSamples = 0;
    TreeMap<Integer, List<Peak>> bandToPeaks = new TreeMap<>();
    Fft fft = new Fft(2048);

    ReferenceSignature() {
        for (int i = 0; i < 2048; i++) {
            int n = 2050;
            int j = i + 1;
            window[i] = 0.5 - 0.5 * Math.cos(2.0 * Math.PI * j / (n - 1));
        }
    }

    static int floorMod(int v, int m) {
        int r = v % m;
        return r < 0 ? r + m : r;
    }

    int peakCount() {
        int s = 0;
        for (List<Peak> l : bandToPeaks.values()) s += l.size();
        return s;
    }

    String[] next() {
        if (pending.length - processed < 128) return null;
        while (pending.length - processed >= 128
                && ((double) signatureNumberSamples / SAMPLE_RATE < MAX_TIME || peakCount() < MAX_PEAKS)) {
            signatureNumberSamples += 128;
            doFft(processed);
            doPeakSpreading();
            if (spreadWritten >= 46) doPeakRecognition();
            processed += 128;
        }
        if (bandToPeaks.isEmpty()) return null;
        return new String[] {
            "data:audio/vnd.shazam.sig;base64," + Base64.getEncoder().encodeToString(encode()),
            Long.toString((signatureNumberSamples * 1000L) / SAMPLE_RATE)
        };
    }

    void doFft(int start) {
        for (int i = start; i < start + 128; i++) {
            ring[ringPos] = pending[i];
            ringPos++;
            if (ringPos == ring.length) ringPos = 0;
        }
        int p = ringPos;
        for (int idx = 0; idx < 2048; idx++) {
            if (p == 2048) p = 0;
            real[idx] = (double) ring[p] * window[idx];
            imag[idx] = 0.0;
            p++;
        }
        fft.fft(real, imag);
        double[] out = fftOutputs[fftPos];
        for (int k = 0; k <= 1024; k++) {
            double re = real[k], im = imag[k];
            double v = (re * re + im * im) / 131072.0;
            out[k] = v <= 1e-10 ? 1e-10 : v;
        }
        fftPos++;
        if (fftPos == fftOutputs.length) fftPos = 0;
    }

    void doPeakSpreading() {
        double[] origin = fftOutputs[floorMod(fftPos - 1, 256)];
        double[] spread = new double[1025];
        for (int i = 0; i <= 1021; i++) spread[i] = Math.max(origin[i], Math.max(origin[i + 1], origin[i + 2]));
        spread[1022] = origin[1022];
        spread[1023] = origin[1023];
        spread[1024] = origin[1024];
        double[] s1 = spreadOutputs[floorMod(spreadPos - 1, 256)];
        double[] s2 = spreadOutputs[floorMod(spreadPos - 3, 256)];
        double[] s3 = spreadOutputs[floorMod(spreadPos - 6, 256)];
        for (int bin = 0; bin < 1025; bin++) {
            double m1 = Math.max(spread[bin], s1[bin]);
            s1[bin] = m1;
            double m2 = Math.max(m1, s2[bin]);
            s2[bin] = m2;
            double m3 = Math.max(m2, s3[bin]);
            s3[bin] = m3;
        }
        System.arraycopy(spread, 0, spreadOutputs[spreadPos], 0, 1025);
        spreadPos++;
        if (spreadPos == 256) spreadPos = 0;
        spreadWritten++;
    }

    void doPeakRecognition() {
        double[] f46 = fftOutputs[floorMod(fftPos - 46, 256)];
        double[] s49 = spreadOutputs[floorMod(spreadPos - 49, 256)];
        int[] offsets = {-10, -7, -4, -3, 1, 2, 5, 8};
        int[] timeOffsets = {-53, -45, 165, 172, 179, 186, 193, 200, 214, 221, 228, 235, 242, 249};
        for (int bin = 10; bin <= 1014; bin++) {
            double energy = f46[bin];
            if (energy >= (1.0 / 64.0) && energy >= s49[bin - 1]) {
                double maxNeighbor = 0.0;
                for (int o : offsets) maxNeighbor = Math.max(maxNeighbor, s49[bin + o]);
                if (energy > maxNeighbor) {
                    double maxTime = maxNeighbor;
                    for (int o : timeOffsets) maxTime = Math.max(maxTime, spreadOutputs[floorMod(spreadPos + o, 256)][bin - 1]);
                    if (energy > maxTime) {
                        int fftNumber = spreadWritten - 46;
                        double mag = Math.log(Math.max(1.0 / 64.0, energy)) * 1477.3 + 6144.0;
                        double before = Math.log(Math.max(1.0 / 64.0, f46[bin - 1])) * 1477.3 + 6144.0;
                        double after = Math.log(Math.max(1.0 / 64.0, f46[bin + 1])) * 1477.3 + 6144.0;
                        double v1 = mag * 2.0 - before - after;
                        if (v1 > 0.0) {
                            double v2 = (after - before) * 32.0 / v1;
                            double corrected = bin * 64.0 + v2;
                            double freq = corrected * (SAMPLE_RATE / 2.0 / 1024.0 / 64.0);
                            Integer band = null;
                            if (freq >= 250.0 && freq <= 520.0) band = 0;
                            else if (freq > 520.0 && freq <= 1450.0) band = 1;
                            else if (freq > 1450.0 && freq <= 3500.0) band = 2;
                            else if (freq > 3500.0 && freq <= 5500.0) band = 3;
                            if (band != null) {
                                bandToPeaks.computeIfAbsent(band, k -> new ArrayList<>())
                                    .add(new Peak(fftNumber, (int) mag, (int) corrected));
                            }
                        }
                    }
                }
            }
        }
    }

    static void le32(ByteArrayOutputStream o, int v) {
        o.write(v & 0xFF);
        o.write((v >>> 8) & 0xFF);
        o.write((v >>> 16) & 0xFF);
        o.write((v >>> 24) & 0xFF);
    }

    static void le16(ByteArrayOutputStream o, int v) {
        o.write(v & 0xFF);
        o.write((v >>> 8) & 0xFF);
    }

    byte[] encode() {
        ByteArrayOutputStream contents = new ByteArrayOutputStream();
        for (Map.Entry<Integer, List<Peak>> e : bandToPeaks.entrySet()) {
            ByteArrayOutputStream pb = new ByteArrayOutputStream();
            int last = 0;
            for (Peak p : e.getValue()) {
                int delta = p.fftPassNumber() - last;
                if (delta >= 255) {
                    pb.write(0xFF);
                    le32(pb, p.fftPassNumber());
                    last = p.fftPassNumber();
                    delta = 0;
                }
                pb.write(Math.min(254, Math.max(0, delta)));
                le16(pb, p.peakMagnitude());
                le16(pb, p.correctedPeakFrequencyBin());
                last = p.fftPassNumber();
            }
            byte[] pv = pb.toByteArray();
            le32(contents, 0x60030040 + e.getKey());
            le32(contents, pv.length);
            contents.write(pv, 0, pv.length);
            int pad = floorMod(-pv.length, 4);
            for (int i = 0; i < pad; i++) contents.write(0);
        }
        byte[] cv = contents.toByteArray();
        int sizeMinusHeader = cv.length + 8;
        ByteBuffer h = ByteBuffer.allocate(48).order(ByteOrder.LITTLE_ENDIAN);
        h.putInt(0xCAFE2580);
        h.putInt(0);
        h.putInt(sizeMinusHeader);
        h.putInt(0x94119C00);
        h.putInt(0);
        h.putInt(0);
        h.putInt(0);
        h.putInt(3 << 27);
        h.putInt(0);
        h.putInt(0);
        h.putInt((int) (signatureNumberSamples + SAMPLE_RATE * 0.24));
        h.putInt((15 << 19) + 0x40000);
        ByteArrayOutputStream full = new ByteArrayOutputStream();
        full.write(h.array(), 0, 48);
        le32(full, 0x40000000);
        le32(full, sizeMinusHeader);
        full.write(cv, 0, cv.length);
        byte[] w = full.toByteArray();
        CRC32 crc = new CRC32();
        crc.update(w, 8, w.length - 8);
        ByteBuffer.wrap(w).order(ByteOrder.LITTLE_ENDIAN).putInt(4, (int) crc.getValue());
        return w;
    }

    static class Fft {
        final int n;
        final double[] cos, sin;
        final int[] rev;

        Fft(int n) {
            this.n = n;
            cos = new double[n / 2];
            sin = new double[n / 2];
            for (int i = 0; i < n / 2; i++) {
                cos[i] = Math.cos(2.0 * Math.PI * i / n);
                sin[i] = Math.sin(2.0 * Math.PI * i / n);
            }
            rev = new int[n];
            int bits = Integer.numberOfTrailingZeros(n);
            for (int i = 0; i < n; i++) {
                int x = i, y = 0;
                for (int b = 0; b < bits; b++) {
                    y = (y << 1) | (x & 1);
                    x >>>= 1;
                }
                rev[i] = y;
            }
        }

        void fft(double[] re, double[] im) {
            for (int i = 0; i < n; i++) {
                int j = rev[i];
                if (j > i) {
                    double t = re[i]; re[i] = re[j]; re[j] = t;
                    t = im[i]; im[i] = im[j]; im[j] = t;
                }
            }
            for (int len = 2; len <= n; len <<= 1) {
                int half = len / 2, step = n / len;
                for (int i = 0; i < n; i += len) {
                    for (int j = 0, k = 0; j < half; j++, k += step) {
                        double c = cos[k], s = sin[k];
                        int i1 = i + j, i2 = i1 + half;
                        double r2 = re[i2], m2 = im[i2];
                        double tpre = r2 * c + m2 * s;
                        double tpim = -r2 * s + m2 * c;
                        re[i2] = re[i1] - tpre;
                        im[i2] = im[i1] - tpim;
                        re[i1] += tpre;
                        im[i1] += tpim;
                    }
                }
            }
        }
    }

    public static void main(String[] args) throws Exception {
        byte[] raw = Files.readAllBytes(Path.of(args[0]));
        ReferenceSignature g = new ReferenceSignature();
        g.pending = new int[raw.length / 2];
        ByteBuffer b = ByteBuffer.wrap(raw).order(ByteOrder.LITTLE_ENDIAN);
        for (int i = 0; i < g.pending.length; i++) g.pending[i] = b.getShort();
        String[] sig = g.next();
        System.out.println(sig == null ? "none" : sig[0]);
        System.out.println(sig == null ? "0" : sig[1]);
    }
}
