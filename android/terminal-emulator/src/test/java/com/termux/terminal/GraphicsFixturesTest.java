package com.termux.terminal;

import com.termux.terminal.PlaceholderRowScanner.Run;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.zip.DataFormatException;
import java.util.zip.Inflater;

public class GraphicsFixturesTest extends TerminalTestCase {

    private static final int HIGH_KEY = 0x2a123456;
    private static final String BORDER = "│";

    private void replay(String fixture) throws IOException {
        byte[] bytes;
        try (InputStream in = GraphicsFixturesTest.class.getResourceAsStream("/graphics/" + fixture)) {
            assertNotNull("Missing fixture " + fixture, in);
            bytes = in.readAllBytes();
        }
        mTerminal.append(bytes, bytes.length);
        assertInvariants();
        assertEquals("", mOutput.getOutputAndClear());
    }

    private ImageStore.Image assertImage(int id, int width, int height, int format, boolean compressed) {
        ImageStore.Image image = mTerminal.getImages().get(id);
        assertNotNull("No image 0x" + Integer.toHexString(id), image);
        assertEquals(width, image.mWidth);
        assertEquals(height, image.mHeight);
        assertEquals(format, image.mFormat);
        assertEquals(compressed, image.mCompressed);
        return image;
    }

    private static void assertPlacement(ImageStore.Image image, int columns, int rows) {
        assertEquals(1, image.getPlacements().size());
        ImageStore.VirtualPlacement placement = image.getPlacement(0);
        assertEquals(0, placement.mPlacementId);
        assertEquals(columns, placement.mColumns);
        assertEquals(rows, placement.mRows);
    }

    private List<Run> runs(int externalRow) {
        TerminalBuffer screen = mTerminal.getScreen();
        TerminalRow row = screen.allocateFullLineIfNecessary(screen.externalToInternalRow(externalRow));
        List<Run> runs = new ArrayList<>();
        PlaceholderRowScanner.scan(row, mTerminal.mColumns, (screenColumn, count, imageId, imageRow, imageColumn) ->
            runs.add(new Run(screenColumn, count, imageId, imageRow, imageColumn)));
        return runs;
    }

    private void assertRuns(int externalRow, Run... expected) {
        assertEquals("Row " + externalRow, Arrays.asList(expected), runs(externalRow));
    }

    private static byte[] noise(int length) {
        byte[] data = new byte[length];
        int state = 1;
        for (int i = 0; i < length; i++) {
            state = state * 1103515245 + 12345;
            data[i] = (byte) (state >>> 16);
        }
        return data;
    }

    private static byte[] inflate(byte[] data) throws DataFormatException {
        Inflater inflater = new Inflater();
        try {
            inflater.setInput(data);
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            byte[] buffer = new byte[4096];
            while (!inflater.finished()) {
                int count = inflater.inflate(buffer);
                if (count == 0 && inflater.needsInput()) fail("Truncated compressed image data");
                out.write(buffer, 0, count);
            }
            return out.toByteArray();
        } finally {
            inflater.end();
        }
    }

    public void testPngUploadPlacementAndPlaceholders() throws IOException {
        withTerminalSized(20, 6);
        replay("png-upload.txt");
        assertPlacement(assertImage(1, 20, 40, ImageStore.FORMAT_PNG, false), 4, 2);
        assertEquals(1, mTerminal.getImages().size());
        assertLineStartsWith(0, 't', 'i', 'n', 'y');
        assertRuns(0);
        assertRuns(1, new Run(0, 4, 1, 0, 0));
        assertRuns(2, new Run(0, 4, 1, 1, 0));
        assertRuns(3);
        assertRuns(4);
        assertRuns(5);
        assertCursorAt(2, 4);
    }

    public void testRawRgbaArrivesCompressedInSeveralChunks() throws IOException, DataFormatException {
        withTerminalSized(20, 6);
        replay("rgba-chunks.txt");
        ImageStore.Image image = assertImage(1, 48, 32, ImageStore.FORMAT_RGBA, true);
        assertPlacement(image, 6, 3);
        assertTrue(Arrays.equals(noise(48 * 32 * 4), inflate(image.mPayload)));
        assertRuns(0);
        assertRuns(1, new Run(2, 6, 1, 0, 0));
        assertRuns(2, new Run(2, 6, 1, 1, 0));
        assertRuns(3, new Run(2, 6, 1, 2, 0));
        assertRuns(4);
        assertRuns(5);
        assertCursorAt(3, 8);
    }

    public void testPlaceholdersStopAtTheBorderAndTheWindowEdges() throws IOException {
        withTerminalSized(21, 6);
        replay("clipped.txt");
        assertPlacement(assertImage(1, 2, 2, ImageStore.FORMAT_RGB, true), 5, 2);
        assertPlacement(assertImage(2, 2, 2, ImageStore.FORMAT_RGB, true), 5, 2);
        assertRuns(0);
        assertRuns(1, new Run(7, 3, 1, 0, 0));
        assertRuns(2, new Run(7, 3, 1, 1, 0));
        assertRuns(3);
        assertRuns(4);
        assertRuns(5, new Run(18, 3, 2, 0, 0));
        for (int row = 0; row < 6; row++)
            assertEquals("Row " + row, BORDER, mTerminal.getScreen().getSelectedText(10, row, 10, row));
        assertCursorAt(1, 7);
    }

    public void testKeyAboveTwoToTheTwentyFourUsesTheThirdDiacritic() throws IOException {
        withTerminalSized(12, 3);
        replay("high-key.txt");
        assertPlacement(assertImage(HIGH_KEY, 20, 40, ImageStore.FORMAT_PNG, false), 3, 1);
        assertForegroundColorAt(1, 1, 0xff123456);
        assertRuns(0);
        assertRuns(1, new Run(1, 3, HIGH_KEY, 0, 0));
        assertRuns(2);
    }

    public void testDeletedImageLeavesTheStoreAndTheScreen() throws IOException {
        withTerminalSized(12, 3);
        replay("delete-shown.txt");
        assertPlacement(assertImage(1, 1, 1, ImageStore.FORMAT_RGB, true), 2, 1);
        assertRuns(0, new Run(0, 2, 1, 0, 0));

        replay("delete-gone.txt");
        assertNull(mTerminal.getImages().get(1));
        assertEquals(0, mTerminal.getImages().size());
        assertRuns(0);
        assertRuns(1);
        assertRuns(2);
    }

}
