package com.termux.terminal;

import java.io.ByteArrayOutputStream;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import java.util.Base64;
import java.util.zip.CRC32;
import java.util.zip.Deflater;

public class KittyGraphicsTest extends TerminalTestCase {

    private static final String RGB_PIXEL = base64(new byte[]{1, 2, 3});

    @Override
    protected void setUp() throws Exception {
        super.setUp();
        withTerminalSized(10, 4);
    }

    private static String apc(String control, String payload) {
        return "\033_G" + control + ";" + payload + "\033\\";
    }

    private static String apc(String control) {
        return "\033_G" + control + "\033\\";
    }

    private static String ok(String keys) {
        return "\033_G" + keys + ";OK\033\\";
    }

    private static String base64(byte[] data) {
        return Base64.getEncoder().encodeToString(data);
    }

    private static byte[] pixels(int length) {
        byte[] data = new byte[length];
        for (int i = 0; i < length; i++) data[i] = (byte) (i * 7 + 1);
        return data;
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

    private static byte[] png(int width, int height) {
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        out.write(new byte[]{(byte) 0x89, 'P', 'N', 'G', '\r', '\n', 0x1A, '\n'}, 0, 8);
        byte[] header = ByteBuffer.allocate(13).putInt(width).putInt(height).put((byte) 8).put((byte) 6).array();
        writePngChunk(out, "IHDR", header);
        writePngChunk(out, "IDAT", deflate(new byte[height * (width * 4 + 1)]));
        writePngChunk(out, "IEND", new byte[0]);
        return out.toByteArray();
    }

    private static void writePngChunk(ByteArrayOutputStream out, String type, byte[] data) {
        byte[] typeBytes = type.getBytes(StandardCharsets.US_ASCII);
        CRC32 crc = new CRC32();
        crc.update(typeBytes);
        crc.update(data);
        byte[] chunk = ByteBuffer.allocate(12 + data.length).putInt(data.length).put(typeBytes).put(data).putInt((int) crc.getValue()).array();
        out.write(chunk, 0, chunk.length);
    }

    private ImageStore images() {
        return mTerminal.getImages();
    }

    private void assertNoResponse(String input) {
        assertEnteringStringGivesResponse(input, "");
    }

    private void assertError(String input, String keys, String code) {
        enterString(input);
        String response = mOutput.getOutputAndClear();
        assertTrue(response, response.startsWith("\033_G" + keys + ";" + code + ":"));
        assertTrue(response, response.endsWith("\033\\"));
        assertEquals(response, -1, response.indexOf("\033_G", 1));
    }

    private void store(int id, String keys) {
        assertNoResponse(apc("a=t,f=24,s=1,v=1,q=2,i=" + id + keys, RGB_PIXEL));
        assertNotNull(images().get(id));
    }

    private void place(int id, int placementId) {
        assertNoResponse(apc("a=p,U=1,q=2,i=" + id + ",p=" + placementId));
    }

    private int placementCount(int id) {
        return images().get(id).getPlacements().size();
    }

    public void testQueryRepliesWithoutStoring() {
        assertEnteringStringGivesResponse(apc("i=31,s=1,v=1,a=q,t=d,f=24", "AAAA"), ok("i=31"));
        assertEquals(0, images().size());
        assertLinesAre("          ", "          ", "          ", "          ");
        assertCursorAt(0, 0);

        assertError(apc("a=q,i=5,f=24,s=2,v=2", "AAAA"), "i=5", "ENODATA");
        assertError(apc("a=q,i=5,t=f,f=24,s=1,v=1", "AAAA"), "i=5", "EINVAL");
        assertNoResponse(apc("a=q,I=5,f=24,s=1,v=1", "AAAA"));
        assertNoResponse(apc("a=q,i=5,p=3,q=1,f=24,s=1,v=1", "AAAA"));
        assertEquals(0, images().size());
    }

    public void testQuietLevels() {
        assertEnteringStringGivesResponse(apc("a=t,f=24,s=1,v=1,i=1", RGB_PIXEL), ok("i=1"));
        assertNoResponse(apc("a=t,f=24,s=1,v=1,i=1,q=1", RGB_PIXEL));
        assertNoResponse(apc("a=t,f=24,s=1,v=1,i=1,q=2", RGB_PIXEL));
        assertNoResponse(apc("a=t,f=24,s=1,v=1,i=1,q=3", RGB_PIXEL));

        assertError(apc("a=t,f=24,s=2,v=1,i=1", RGB_PIXEL), "i=1", "ENODATA");
        assertError(apc("a=t,f=24,s=2,v=1,i=1,q=1", RGB_PIXEL), "i=1", "ENODATA");
        assertNoResponse(apc("a=t,f=24,s=2,v=1,i=1,q=2", RGB_PIXEL));

        assertEnteringStringGivesResponse(apc("a=p,U=1,i=1"), ok("i=1"));
        assertNoResponse(apc("a=p,U=1,i=1,q=1"));
        assertNoResponse(apc("a=p,U=1,i=1,q=2"));
        assertError(apc("a=p,U=1,i=9,q=1"), "i=9", "ENOENT");
        assertNoResponse(apc("a=p,U=1,i=9,q=2"));
    }

    public void testNoReplyWithoutIdOrNumber() {
        assertNoResponse(apc("a=t,f=24,s=1,v=1", RGB_PIXEL));
        assertNoResponse(apc("a=T,U=1,f=24,s=1,v=1", RGB_PIXEL));
        assertNoResponse(apc("a=t,f=24,s=2,v=2", RGB_PIXEL));
        assertNoResponse(apc("a=t,t=f,f=24,s=1,v=1", RGB_PIXEL));
        assertNoResponse(apc("a=q,f=24,s=1,v=1", RGB_PIXEL));
        assertNoResponse(apc("a=p,U=1"));
        assertNoResponse(apc("a=x"));
        assertEquals(0, images().size());
    }

    public void testExactErrorMessages() {
        assertEnteringStringGivesResponse(apc("a=t,f=24,s=2,v=2,i=1", RGB_PIXEL), "\033_Gi=1;ENODATA:Insufficient image data: 3 < 12\033\\");
        assertEnteringStringGivesResponse(apc("a=p,U=1,i=4,p=2"), "\033_Gi=4,p=2;ENOENT:Put command refers to non-existent image with id: 4 and number: 0\033\\");
        assertEnteringStringGivesResponse(apc("a=t,i=4294967295,f=8"), "\033_Gi=4294967295;EINVAL:Unknown image format: 8\033\\");
    }

    public void testImageNumberGetsSmallestFreeId() {
        store(1, "");
        store(3, "");
        assertEnteringStringGivesResponse(apc("a=t,f=24,s=1,v=1,I=7", RGB_PIXEL), ok("i=2,I=7"));
        assertEnteringStringGivesResponse(apc("a=t,f=24,s=1,v=1,I=7", RGB_PIXEL), ok("i=4,I=7"));
        assertEquals(4, images().findByNumber(7).mId);
        assertEquals(7, images().get(2).mNumber);

        assertEnteringStringGivesResponse(apc("a=p,U=1,I=7,p=3"), ok("i=4,I=7,p=3"));
        assertEquals(1, placementCount(4));
        assertEquals(0, placementCount(2));

        assertError(apc("a=t,f=24,s=1,v=1,i=1,I=7", RGB_PIXEL), "i=1,I=7", "EINVAL");
        assertError(apc("a=t,f=24,s=2,v=2,I=8", RGB_PIXEL), "I=8", "ENODATA");
        assertError(apc("a=p,U=1,I=8"), "I=8", "ENOENT");
    }

    public void testChunkedTransmissionAtEveryBoundary() {
        byte[] data = pixels(5 * 4);
        String encoded = base64(data);
        assertTrue(encoded.endsWith("="));
        int length = encoded.length();
        for (int first = 0; first <= length; first++) {
            for (int second = first; second <= length; second++) {
                assertNoResponse(apc("a=t,f=32,s=5,v=1,i=9,m=1", encoded.substring(0, first)));
                assertNoResponse(apc("m=1", encoded.substring(first, second)));
                assertEnteringStringGivesResponse(apc("m=0", encoded.substring(second)), ok("i=9"));
                assertTrue(first + "/" + second, Arrays.equals(data, images().get(9).mPayload));
            }
        }
    }

    public void testChunksPaddedAtAlignedBoundaries() {
        byte[] image = png(2, 2);
        assertNoResponse(apc("a=t,f=100,i=2,m=1", base64(Arrays.copyOfRange(image, 0, 10))));
        assertNoResponse(apc("m=1", base64(Arrays.copyOfRange(image, 10, 20))));
        assertEnteringStringGivesResponse(apc("m=0", base64(Arrays.copyOfRange(image, 20, image.length))), ok("i=2"));
        assertTrue(Arrays.equals(image, images().get(2).mPayload));
    }

    public void testContinuationChunksUseFirstCommand() {
        String encoded = base64(pixels(3));
        assertNoResponse(apc("a=t,f=24,s=1,v=1,i=4,p=6,m=1", encoded.substring(0, 2)));
        assertEnteringStringGivesResponse(apc("a=t,f=32,i=99,s=7,v=7,m=0", encoded.substring(2)), ok("i=4,p=6"));
        assertNull(images().get(99));
        ImageStore.Image image = images().get(4);
        assertEquals(ImageStore.FORMAT_RGB, image.mFormat);
        assertEquals(1, image.mWidth);
        assertTrue(Arrays.equals(pixels(3), image.mPayload));

        assertNoResponse(apc("a=t,f=24,s=1,v=1,i=5,m=1", encoded.substring(0, 2)));
        assertNoResponse(apc("m=0,q=2", encoded.substring(2)));
        assertNotNull(images().get(5));
    }

    public void testRawFormats() {
        byte[] rgb = pixels(6);
        assertEnteringStringGivesResponse(apc("a=t,f=24,s=2,v=1,i=1", base64(rgb)), ok("i=1"));
        ImageStore.Image image = images().get(1);
        assertEquals(2, image.mWidth);
        assertEquals(1, image.mHeight);
        assertEquals(ImageStore.FORMAT_RGB, image.mFormat);
        assertFalse(image.mCompressed);
        assertTrue(Arrays.equals(rgb, image.mPayload));

        byte[] rgba = pixels(8);
        assertEnteringStringGivesResponse(apc("a=t,f=32,s=1,v=2,i=2", base64(rgba)), ok("i=2"));
        assertEquals(ImageStore.FORMAT_RGBA, images().get(2).mFormat);
        assertEquals(2, images().get(2).mHeight);
        assertTrue(Arrays.equals(rgba, images().get(2).mPayload));

        assertEnteringStringGivesResponse(apc("s=1,v=2,i=3", base64(rgba)), ok("i=3"));
        assertEquals(ImageStore.FORMAT_RGBA, images().get(3).mFormat);

        assertEnteringStringGivesResponse(apc("a=t,f=24,s=1,v=1,i=4", base64(pixels(5))), ok("i=4"));
        assertTrue(Arrays.equals(pixels(3), images().get(4).mPayload));
    }

    public void testPng() {
        byte[] image = png(3, 2);
        assertEnteringStringGivesResponse(apc("a=t,f=100,i=1,s=50,v=50", base64(image)), ok("i=1"));
        ImageStore.Image stored = images().get(1);
        assertEquals(3, stored.mWidth);
        assertEquals(2, stored.mHeight);
        assertEquals(ImageStore.FORMAT_PNG, stored.mFormat);
        assertFalse(stored.mCompressed);
        assertTrue(Arrays.equals(image, stored.mPayload));

        assertError(apc("a=t,f=100,i=2", base64("not a png image at all!!".getBytes(StandardCharsets.US_ASCII))), "i=2", "EBADPNG");
        assertError(apc("a=t,f=100,i=2", base64(Arrays.copyOf(image, 20))), "i=2", "EBADPNG");
        byte[] missingHeader = image.clone();
        missingHeader[12] = 'X';
        assertError(apc("a=t,f=100,i=2", base64(missingHeader)), "i=2", "EBADPNG");
        assertError(apc("a=t,f=100,i=2", base64(png(0, 2))), "i=2", "EBADPNG");
        assertError(apc("a=t,f=100,i=2", base64(png(10001, 1))), "i=2", "EINVAL");
        assertError(apc("a=t,f=100,i=2", ""), "i=2", "EBADPNG");
        assertNull(images().get(2));
    }

    public void testCompressed() {
        byte[] rgba = pixels(4 * 4 * 4);
        byte[] compressed = deflate(rgba);
        assertEnteringStringGivesResponse(apc("a=t,f=32,o=z,s=4,v=4,i=1", base64(compressed)), ok("i=1"));
        ImageStore.Image image = images().get(1);
        assertTrue(image.mCompressed);
        assertEquals(4, image.mWidth);
        assertTrue(Arrays.equals(compressed, image.mPayload));

        String encoded = base64(deflate(pixels(3 * 3 * 3)));
        assertNoResponse(apc("a=t,f=24,o=z,s=3,v=3,i=2,m=1", encoded.substring(0, 5)));
        assertNoResponse(apc("m=1", encoded.substring(5, 11)));
        assertEnteringStringGivesResponse(apc("m=0", encoded.substring(11)), ok("i=2"));
        assertTrue(images().get(2).mCompressed);

        assertError(apc("a=t,f=32,o=z,s=4,v=4,i=3", base64(deflate(pixels(63)))), "i=3", "EINVAL");
        assertError(apc("a=t,f=32,o=z,s=4,v=4,i=3", base64(deflate(pixels(65)))), "i=3", "EINVAL");
        assertError(apc("a=t,f=32,o=z,s=4,v=4,i=3", base64(rgba)), "i=3", "EINVAL");
        assertError(apc("a=t,f=32,o=z,s=4,v=4,i=3", base64(Arrays.copyOf(compressed, compressed.length - 3))), "i=3", "EINVAL");
        assertError(apc("a=t,f=32,o=z,s=4,v=4,i=3", ""), "i=3", "EINVAL");
        assertError(apc("a=t,f=32,o=x,s=4,v=4,i=3", base64(compressed)), "i=3", "EINVAL");
        assertNull(images().get(3));

        byte[] pngImage = png(5, 7);
        byte[] compressedPng = deflate(pngImage);
        assertEnteringStringGivesResponse(apc("a=t,f=100,o=z,i=4", base64(compressedPng)), ok("i=4"));
        assertEquals(5, images().get(4).mWidth);
        assertEquals(7, images().get(4).mHeight);
        assertTrue(images().get(4).mCompressed);
        assertTrue(Arrays.equals(compressedPng, images().get(4).mPayload));
        assertError(apc("a=t,f=100,o=z,i=5", base64(deflate(new byte[10]))), "i=5", "EBADPNG");
    }

    public void testValidationErrors() {
        assertError(apc("a=t,f=8,s=1,v=1,i=1", RGB_PIXEL), "i=1", "EINVAL");
        assertError(apc("a=t,t=f,f=24,s=1,v=1,i=1", RGB_PIXEL), "i=1", "EINVAL");
        assertError(apc("a=t,t=t,f=24,s=1,v=1,i=1", RGB_PIXEL), "i=1", "EINVAL");
        assertError(apc("a=t,t=s,f=24,s=1,v=1,i=1", RGB_PIXEL), "i=1", "EINVAL");
        assertError(apc("a=t,f=24,v=1,i=1", RGB_PIXEL), "i=1", "EINVAL");
        assertError(apc("a=t,f=24,s=1,i=1", RGB_PIXEL), "i=1", "EINVAL");
        assertError(apc("a=t,f=24,s=10001,v=1,i=1", RGB_PIXEL), "i=1", "EINVAL");
        assertError(apc("a=t,f=24,s=1,v=4294967295,i=1", RGB_PIXEL), "i=1", "EINVAL");
        assertError(apc("a=t,f=24,s=1,v=1,i=1", "!!!!"), "i=1", "EINVAL");
        assertError(apc("a=t,f=24,s=1,v=1,i=1", "AAAAA"), "i=1", "EINVAL");
        assertError(apc("a=t,f=32,s=4000,v=4000,i=1", RGB_PIXEL), "i=1", "EFBIG");
        assertError(apc("a=T,f=24,s=1,v=1,i=1", RGB_PIXEL), "i=1", "EINVAL");
        assertError(apc("a=T,U=1,c=10001,f=24,s=1,v=1,i=1", RGB_PIXEL), "i=1", "EINVAL");
        assertError(apc("a=f,i=1", RGB_PIXEL), "i=1", "EINVAL");
        assertError(apc("a=a,i=1"), "i=1", "EINVAL");
        assertEquals(0, images().size());
    }

    public void testTooMuchData() {
        ImageStore store = new ImageStore(16);
        KittyGraphics graphics = new KittyGraphics(store, mOutput);
        graphics.handle("a=t,f=100,i=1,m=1;" + base64(new byte[12]));
        assertEquals("", mOutput.getOutputAndClear());
        graphics.handle("m=1;" + base64(new byte[12]));
        String response = mOutput.getOutputAndClear();
        assertTrue(response, response.startsWith("\033_Gi=1;EFBIG:"));
        graphics.handle("m=0;" + base64(new byte[12]));
        assertEquals("", mOutput.getOutputAndClear());
        assertEquals(0, store.size());

        graphics.handle("a=t,f=32,s=3,v=2,i=1;" + base64(new byte[24]));
        response = mOutput.getOutputAndClear();
        assertTrue(response, response.startsWith("\033_Gi=1;EFBIG:"));

        graphics.handle("a=t,f=32,o=z,s=3,v=2,i=1;" + base64(deflate(new byte[24])));
        assertEquals(ok("i=1"), mOutput.getOutputAndClear());
    }

    public void testErrorMidTransmissionRepliesOnce() {
        assertNoResponse(apc("a=t,f=24,s=2,v=2,i=3,m=1", "AAAA"));
        assertError(apc("m=1", "!!!!"), "i=3", "EINVAL");
        assertNoResponse(apc("m=1", "AAAA"));
        assertNoResponse(apc("m=0", "AAAA"));
        assertNull(images().get(3));

        assertError(apc("a=t,t=f,i=3,m=1", "AAAA"), "i=3", "EINVAL");
        assertNoResponse(apc("m=1", "AAAA"));
        assertNoResponse(apc("m=0", "AAAA"));

        assertError(apc("a=f,i=3,m=1", "AAAA"), "i=3", "EINVAL");
        assertNoResponse(apc("m=0", "AAAA"));

        assertEnteringStringGivesResponse(apc("a=t,f=24,s=1,v=1,i=3", RGB_PIXEL), ok("i=3"));
    }

    public void testOtherCommandsDuringTransmission() {
        store(2, "");
        store(3, "");
        String encoded = base64(pixels(3));
        assertNoResponse(apc("a=t,f=24,s=1,v=1,i=1,m=1", encoded.substring(0, 2)));
        assertEnteringStringGivesResponse(apc("a=p,U=1,i=2"), ok("i=2"));
        assertNoResponse(apc("a=d,d=I,i=3"));
        assertNull(images().get(3));
        assertEnteringStringGivesResponse(apc("m=0", encoded.substring(2)), ok("i=1"));
        assertTrue(Arrays.equals(pixels(3), images().get(1).mPayload));
        assertEquals(1, placementCount(2));
    }

    public void testVirtualPlacements() {
        enterString("ab");
        placeCursorAndAssert(1, 2);
        assertEnteringStringGivesResponse(apc("a=T,U=1,f=24,s=1,v=1,i=1,c=4,r=2", RGB_PIXEL), ok("i=1"));
        assertCursorAt(1, 2);
        ImageStore.Image image = images().get(1);
        assertEquals(1, image.getPlacements().size());
        ImageStore.VirtualPlacement placement = image.getPlacement(0);
        assertEquals(0, placement.mPlacementId);
        assertEquals(4, placement.mColumns);
        assertEquals(2, placement.mRows);

        assertEnteringStringGivesResponse(apc("a=p,U=1,i=1,p=7,c=3,r=1"), ok("i=1,p=7"));
        assertEnteringStringGivesResponse(apc("a=p,U=1,i=1,p=7,c=5,r=6"), ok("i=1,p=7"));
        assertEnteringStringGivesResponse(apc("a=p,U=1,i=1"), ok("i=1"));
        assertEquals(3, image.getPlacements().size());
        assertEquals(5, image.getPlacement(7).mColumns);
        assertEquals(6, image.getPlacement(7).mRows);
        assertCursorAt(1, 2);

        assertError(apc("a=p,i=1,c=1,r=1"), "i=1", "EINVAL");
        assertError(apc("a=p,U=1,i=1,r=10001"), "i=1", "EINVAL");
        assertEquals(3, image.getPlacements().size());
        assertError(apc("a=T,f=24,s=1,v=1,i=2", RGB_PIXEL), "i=2", "EINVAL");
        assertNull(images().get(2));

        assertEnteringStringGivesResponse(apc("a=T,U=1,f=24,s=1,v=1,i=3,p=4", RGB_PIXEL), ok("i=3,p=4"));
        assertEquals(4, images().get(3).getPlacement(0).mPlacementId);
        assertCursorAt(1, 2);
        assertLinesAre("ab        ", "          ", "          ", "          ");
    }

    public void testRetransmitReplacesImage() {
        assertNoResponse(apc("a=T,U=1,q=2,f=24,s=1,v=1,i=1", RGB_PIXEL));
        ImageStore.Image first = images().get(1);
        assertTrue(first.hasPlacements());
        int modCount = images().getModCount();

        assertNoResponse(apc("a=t,q=2,f=32,s=1,v=1,i=1", base64(pixels(4))));
        ImageStore.Image second = images().get(1);
        assertNotSame(first, second);
        assertTrue(second.mGeneration > first.mGeneration);
        assertFalse(second.hasPlacements());
        assertEquals(ImageStore.FORMAT_RGBA, second.mFormat);
        assertTrue(images().getModCount() != modCount);
        assertEquals(1, images().size());
    }

    public void testDeleteById() {
        store(1, "");
        place(1, 1);
        place(1, 2);
        assertNoResponse(apc("a=d,d=i,i=1,p=2"));
        assertEquals(1, placementCount(1));
        assertNotNull(images().get(1).getPlacement(1));
        assertNoResponse(apc("a=d,d=i,i=1"));
        assertEquals(0, placementCount(1));

        place(1, 1);
        place(1, 2);
        assertNoResponse(apc("a=d,d=I,i=1,p=2"));
        assertEquals(1, placementCount(1));
        assertNoResponse(apc("a=d,d=I,i=1,p=1"));
        assertNull(images().get(1));

        store(5, "");
        assertNoResponse(apc("a=d,d=I,i=5"));
        assertNull(images().get(5));
        assertNoResponse(apc("a=d,d=I,i=6"));
        assertNoResponse(apc("a=d,d=I"));
    }

    public void testDeleteByNumber() {
        store(1, ",I=0");
        assertEnteringStringGivesResponse(apc("a=t,f=24,s=1,v=1,I=7", RGB_PIXEL), ok("i=2,I=7"));
        assertEnteringStringGivesResponse(apc("a=t,f=24,s=1,v=1,I=7", RGB_PIXEL), ok("i=3,I=7"));
        assertNoResponse(apc("a=p,U=1,q=2,i=2"));
        assertNoResponse(apc("a=p,U=1,q=2,i=3,p=1"));
        assertNoResponse(apc("a=p,U=1,q=2,i=3,p=2"));

        assertNoResponse(apc("a=d,d=n,I=7,p=1"));
        assertEquals(1, placementCount(3));
        assertNoResponse(apc("a=d,d=n,I=7"));
        assertEquals(0, placementCount(3));
        assertEquals(1, placementCount(2));
        assertNoResponse(apc("a=d,d=N,I=7"));
        assertNull(images().get(3));
        assertEquals(1, placementCount(2));
        assertNoResponse(apc("a=d,d=N,I=7"));
        assertNull(images().get(2));
        assertNotNull(images().get(1));
    }

    public void testDeleteByRange() {
        for (int id = 1; id <= 5; id++) {
            store(id, "");
            place(id, 1);
        }
        assertNoResponse(apc("a=d,d=r,x=2,y=4"));
        assertEquals(1, placementCount(1));
        assertEquals(0, placementCount(2));
        assertEquals(0, placementCount(4));
        assertEquals(1, placementCount(5));
        assertEquals(5, images().size());

        assertNoResponse(apc("a=d,d=R,x=1,y=3"));
        assertNull(images().get(1));
        assertNull(images().get(2));
        assertNull(images().get(3));
        assertNotNull(images().get(4));
        assertNotNull(images().get(5));
        assertNoResponse(apc("a=d,d=R,x=5,y=4"));
        assertEquals(2, images().size());
    }

    public void testOtherSelectorsKeepVirtualPlacements() {
        store(1, "");
        place(1, 1);
        store(2, "");
        String[] selectors = {"", "d=a", "d=c", "d=C", "d=p,x=1,y=1", "d=P,x=1,y=1", "d=q,x=1,y=1,z=0", "d=Q,x=1,y=1,z=0",
            "d=x,x=1", "d=X,x=1", "d=y,y=1", "d=Y,y=1", "d=z,z=0", "d=Z,z=0", "d=f,i=1", "d=F,i=1", "d=k"};
        for (String selector : selectors) {
            assertNoResponse(apc("a=d,i=1" + (selector.isEmpty() ? "" : "," + selector)));
            assertEquals(selector, 1, placementCount(1));
            assertNotNull(selector, images().get(2));
        }

        assertNoResponse(apc("a=d,d=A"));
        assertEquals(1, placementCount(1));
        assertNull(images().get(2));
    }

    public void testProbeSequence() {
        assertEnteringStringGivesResponse("\033_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\033\\\033[>q\033[16t\033[c",
            "\033_Gi=31;OK\033\\" + "\033P>|amux-android(1)\033\\" + "\033[6;15;13t" + "\033[?64;1;2;6;9;15;18;21;22c");
        assertEnteringStringGivesResponse("\033[>0q", "\033P>|amux-android(1)\033\\");
        assertEnteringStringGivesResponse("\033[>1q", "");
        assertLinesAre("          ", "          ", "          ", "          ");
    }

    public void testCellPixelSize() {
        assertEquals(INITIAL_CELL_WIDTH_PIXELS, mTerminal.getCellWidthPixels());
        assertEquals(INITIAL_CELL_HEIGHT_PIXELS, mTerminal.getCellHeightPixels());
        mTerminal.resize(10, 4, 20, 40);
        assertEquals(20, mTerminal.getCellWidthPixels());
        assertEquals(40, mTerminal.getCellHeightPixels());
    }

    public void testApcAtCapIsHandled() {
        String control = "a=q,i=1,f=100;";
        StringBuilder body = new StringBuilder(control);
        while (body.length() < ApcBuffer.MAX_LENGTH) body.append('A');
        assertError("\033_G" + body + "\033\\", "i=1", "EBADPNG");
    }

    public void testApcOverCapIsDiscarded() {
        StringBuilder body = new StringBuilder("a=t,f=100,i=1;");
        while (body.length() <= ApcBuffer.MAX_LENGTH) body.append('A');
        assertNoResponse("\033_G" + body + "\033\\ok");
        assertEquals(0, images().size());
        assertLinesAre("ok        ", "          ", "          ", "          ");

        assertNoResponse("\033_G" + body + body + "\033\\");
        assertEnteringStringGivesResponse(apc("a=t,f=24,s=1,v=1,i=1", RGB_PIXEL), ok("i=1"));
    }

    public void testRisClearsStore() {
        store(1, "");
        place(1, 1);
        String encoded = base64(pixels(3));
        assertNoResponse(apc("a=t,f=24,s=1,v=1,i=2,m=1", encoded.substring(0, 2)));
        int modCount = images().getModCount();
        enterString("\033c");
        assertEquals(0, images().size());
        assertTrue(images().getModCount() != modCount);
        assertNoResponse(apc("m=0", encoded.substring(2)));
        assertNull(images().get(2));
        assertEquals(0, images().size());
    }

    public void testSoftResetKeepsStore() {
        store(1, "");
        enterString("\033[!p");
        assertNotNull(images().get(1));
    }

}
