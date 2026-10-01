package com.termux.view;

import com.termux.terminal.ImageStore;

import java.io.ByteArrayInputStream;
import java.io.DataInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.util.zip.InflaterInputStream;

final class Pixels {

    interface RowConsumer {
        void accept(int row, int[] argb);
    }

    private Pixels() {
    }

    static InputStream open(byte[] payload, boolean compressed) {
        InputStream in = new ByteArrayInputStream(payload);
        return compressed ? new InflaterInputStream(in) : in;
    }

    static int scaled(int length, int step) {
        return (length + step - 1) / step;
    }

    static int sampleStep(int width, int height, int maxSide) {
        int step = 1;
        while (scaled(width, step) > maxSide || scaled(height, step) > maxSide) step <<= 1;
        return step;
    }

    static void decode(byte[] payload, boolean compressed, int width, int height, int format, int step, RowConsumer out) throws IOException {
        if (format != ImageStore.FORMAT_RGB && format != ImageStore.FORMAT_RGBA) throw new IOException("Not a raw pixel format: " + format);
        final int bytesPerPixel = format / 8;
        final byte[] source = new byte[width * bytesPerPixel];
        final int[] argb = new int[scaled(width, step)];
        try (DataInputStream in = new DataInputStream(open(payload, compressed))) {
            for (int row = 0, rows = scaled(height, step); row < rows; row++) {
                if (row > 0) for (int skipped = 1; skipped < step; skipped++) in.readFully(source);
                in.readFully(source);
                toArgb(source, bytesPerPixel, step, argb);
                out.accept(row, argb);
            }
        }
    }

    static void toArgb(byte[] source, int bytesPerPixel, int step, int[] out) {
        final int stride = bytesPerPixel * step;
        for (int x = 0, i = 0; x < out.length; x++, i += stride) {
            final int alpha = bytesPerPixel == 4 ? source[i + 3] & 0xff : 0xff;
            out[x] = alpha << 24 | (source[i] & 0xff) << 16 | (source[i + 1] & 0xff) << 8 | (source[i + 2] & 0xff);
        }
    }

}
