package com.termux.view;

import com.termux.terminal.ImageStore;

import junit.framework.TestCase;

import java.io.ByteArrayOutputStream;
import java.io.EOFException;
import java.io.IOException;
import java.util.ArrayList;
import java.util.List;
import java.util.zip.Deflater;

public class PixelsTest extends TestCase {

    private static byte[] bytes(int... values) {
        byte[] result = new byte[values.length];
        for (int i = 0; i < values.length; i++) result[i] = (byte) values[i];
        return result;
    }

    private static byte[] deflate(byte[] data) {
        Deflater deflater = new Deflater();
        deflater.setInput(data);
        deflater.finish();
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        byte[] buffer = new byte[1024];
        while (!deflater.finished()) {
            int count = deflater.deflate(buffer);
            out.write(buffer, 0, count);
        }
        deflater.end();
        return out.toByteArray();
    }

    private static int[][] decode(byte[] payload, boolean compressed, int width, int height, int format, int step) throws IOException {
        List<int[]> rows = new ArrayList<>();
        Pixels.decode(payload, compressed, width, height, format, step, (row, argb) -> {
            assertEquals(rows.size(), row);
            rows.add(argb.clone());
        });
        return rows.toArray(new int[0][]);
    }

    public void testRgbBecomesOpaqueArgb() throws IOException {
        int[][] rows = decode(bytes(1, 2, 3, 250, 251, 252, 0, 0, 0, 255, 255, 255), false, 2, 2, ImageStore.FORMAT_RGB, 1);
        assertEquals(2, rows.length);
        assertArrayEquals(new int[]{0xff010203, 0xfffafbfc}, rows[0]);
        assertArrayEquals(new int[]{0xff000000, 0xffffffff}, rows[1]);
    }

    public void testRgbaKeepsStraightAlpha() throws IOException {
        int[][] rows = decode(bytes(10, 20, 30, 0x80, 255, 0, 0, 0, 1, 2, 3, 255), false, 3, 1, ImageStore.FORMAT_RGBA, 1);
        assertEquals(1, rows.length);
        assertArrayEquals(new int[]{0x800a141e, 0x00ff0000, 0xff010203}, rows[0]);
    }

    public void testCompressedPayloadInflates() throws IOException {
        byte[] raw = new byte[3 * 2 * 4];
        for (int i = 0; i < raw.length; i++) raw[i] = (byte) (i * 11 + 5);
        int[][] expected = decode(raw, false, 3, 2, ImageStore.FORMAT_RGBA, 1);
        int[][] inflated = decode(deflate(raw), true, 3, 2, ImageStore.FORMAT_RGBA, 1);
        assertEquals(expected.length, inflated.length);
        for (int row = 0; row < expected.length; row++) assertArrayEquals(expected[row], inflated[row]);
    }

    public void testShortPayloadFails() {
        byte[] raw = new byte[2 * 2 * 3 - 1];
        try {
            decode(raw, false, 2, 2, ImageStore.FORMAT_RGB, 1);
            fail();
        } catch (IOException e) {
            assertTrue(e instanceof EOFException);
        }
        try {
            decode(deflate(raw), true, 2, 2, ImageStore.FORMAT_RGB, 1);
            fail();
        } catch (IOException e) {
            assertTrue(e instanceof EOFException);
        }
    }

    public void testCorruptCompressedPayloadFails() {
        try {
            decode(bytes(1, 2, 3, 4, 5, 6), true, 1, 1, ImageStore.FORMAT_RGB, 1);
            fail();
        } catch (IOException expected) {
        }
    }

    public void testExtraBytesAreIgnored() throws IOException {
        int[][] rows = decode(bytes(1, 2, 3, 4, 5, 6, 7), false, 1, 2, ImageStore.FORMAT_RGB, 1);
        assertArrayEquals(new int[]{0xff010203}, rows[0]);
        assertArrayEquals(new int[]{0xff040506}, rows[1]);
    }

    public void testSamplingKeepsEveryStepthPixel() throws IOException {
        int width = 5, height = 3;
        byte[] raw = new byte[width * height * 3];
        for (int y = 0; y < height; y++) {
            for (int x = 0; x < width; x++) {
                int i = (y * width + x) * 3;
                raw[i] = (byte) x;
                raw[i + 1] = (byte) y;
            }
        }
        int[][] rows = decode(deflate(raw), true, width, height, ImageStore.FORMAT_RGB, 2);
        assertEquals(2, rows.length);
        assertArrayEquals(new int[]{0xff000000, 0xff020000, 0xff040000}, rows[0]);
        assertArrayEquals(new int[]{0xff000200, 0xff020200, 0xff040200}, rows[1]);
    }

    public void testUnsupportedFormatFails() {
        try {
            decode(new byte[16], false, 2, 2, ImageStore.FORMAT_PNG, 1);
            fail();
        } catch (IOException expected) {
        }
    }

    public void testSampleStep() {
        assertEquals(1, Pixels.sampleStep(4096, 4096, 4096));
        assertEquals(2, Pixels.sampleStep(4097, 10, 4096));
        assertEquals(4, Pixels.sampleStep(100, 9000, 4096));
        assertEquals(4, Pixels.sampleStep(10000, 10000, 4096));
        assertEquals(8, Pixels.sampleStep(10000, 1, 2048));
        assertEquals(2, Pixels.scaled(4097, 4096));
        assertEquals(3, Pixels.scaled(5, 2));
    }

    private static void assertArrayEquals(int[] expected, int[] actual) {
        assertEquals(expected.length, actual.length);
        for (int i = 0; i < expected.length; i++) {
            assertEquals("index " + i, Integer.toHexString(expected[i]), Integer.toHexString(actual[i]));
        }
    }

}
