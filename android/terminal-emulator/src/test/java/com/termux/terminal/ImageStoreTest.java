package com.termux.terminal;

import junit.framework.TestCase;

public class ImageStoreTest extends TestCase {

    private static ImageStore.Image put(ImageStore store, int id, int length) {
        return store.put(id, 0, 1, 1, ImageStore.FORMAT_PNG, false, new byte[length]);
    }

    public void testPutAndGet() {
        ImageStore store = new ImageStore(100);
        byte[] payload = {1, 2, 3};
        ImageStore.Image image = store.put(7, 3, 4, 5, ImageStore.FORMAT_RGB, true, payload);
        assertSame(image, store.get(7));
        assertEquals(7, image.mId);
        assertEquals(3, image.mNumber);
        assertEquals(4, image.mWidth);
        assertEquals(5, image.mHeight);
        assertEquals(ImageStore.FORMAT_RGB, image.mFormat);
        assertTrue(image.mCompressed);
        assertSame(payload, image.mPayload);
        assertFalse(image.hasPlacements());
        assertEquals(1, store.size());
        assertEquals(3, store.getSize());
        assertNull(store.get(8));
        assertEquals(ImageStore.DEFAULT_CAPACITY, new ImageStore().getCapacity());
    }

    public void testReplaceBumpsGenerationAndDropsPlacements() {
        ImageStore store = new ImageStore(100);
        ImageStore.Image first = put(store, 1, 10);
        store.place(first, 0, 4, 2);
        ImageStore.Image second = put(store, 1, 30);
        assertSame(second, store.get(1));
        assertTrue(second.mGeneration > first.mGeneration);
        assertFalse(second.hasPlacements());
        assertEquals(1, store.size());
        assertEquals(30, store.getSize());
    }

    public void testGenerationsNeverRepeatAcrossRemoval() {
        ImageStore store = new ImageStore(100);
        int first = put(store, 1, 1).mGeneration;
        assertTrue(store.remove(1));
        assertTrue(put(store, 1, 1).mGeneration > first);
        store.clear();
        assertTrue(put(store, 1, 1).mGeneration > first + 1);
    }

    public void testCapacityEvictsLeastRecentlyUsed() {
        ImageStore store = new ImageStore(100);
        put(store, 1, 40);
        put(store, 2, 40);
        put(store, 3, 40);
        assertNull(store.get(1));
        assertNotNull(store.get(2));
        assertNotNull(store.get(3));
        assertEquals(80, store.getSize());
    }

    public void testTouchKeepsImage() {
        ImageStore store = new ImageStore(100);
        ImageStore.Image first = put(store, 1, 40);
        put(store, 2, 40);
        store.touch(first);
        put(store, 3, 40);
        assertNotNull(store.get(1));
        assertNull(store.get(2));
        assertNotNull(store.get(3));
    }

    public void testPlacingCountsAsUse() {
        ImageStore store = new ImageStore(100);
        ImageStore.Image first = put(store, 1, 40);
        put(store, 2, 40);
        store.place(first, 0, 1, 1);
        put(store, 3, 40);
        assertNotNull(store.get(1));
        assertNull(store.get(2));
    }

    public void testEvictsSeveralForOneLargeImage() {
        ImageStore store = new ImageStore(100);
        put(store, 1, 30);
        put(store, 2, 30);
        put(store, 3, 30);
        put(store, 4, 100);
        assertEquals(1, store.size());
        assertNotNull(store.get(4));
        assertEquals(100, store.getSize());
    }

    public void testPayloadOverCapacityIsRejected() {
        ImageStore store = new ImageStore(100);
        try {
            put(store, 1, 101);
            fail();
        } catch (IllegalArgumentException e) {
            assertEquals(0, store.size());
        }
    }

    public void testModCount() {
        ImageStore store = new ImageStore(100);
        int count = store.getModCount();
        ImageStore.Image image = put(store, 1, 10);
        assertEquals(++count, store.getModCount());

        store.touch(image);
        store.get(1);
        store.findByNumber(0);
        assertEquals(count, store.getModCount());

        store.place(image, 0, 1, 1);
        assertEquals(++count, store.getModCount());
        assertTrue(store.unplace(image, 0));
        assertEquals(++count, store.getModCount());
        assertFalse(store.unplace(image, 0));
        assertEquals(count, store.getModCount());

        assertFalse(store.remove(2));
        assertEquals(count, store.getModCount());
        assertTrue(store.remove(1));
        assertEquals(++count, store.getModCount());

        store.clear();
        assertEquals(count, store.getModCount());
        put(store, 1, 10);
        count = store.getModCount();
        store.clear();
        assertEquals(++count, store.getModCount());
        assertEquals(0, store.getSize());
    }

    public void testPlacements() {
        ImageStore store = new ImageStore(100);
        ImageStore.Image image = put(store, 1, 10);
        store.place(image, 0, 1, 1);
        store.place(image, 0, 2, 2);
        store.place(image, 5, 3, 3);
        store.place(image, 6, 4, 4);
        assertEquals(4, image.getPlacements().size());

        store.place(image, 5, 7, 8);
        assertEquals(4, image.getPlacements().size());
        assertEquals(7, image.getPlacement(5).mColumns);
        assertEquals(8, image.getPlacement(5).mRows);
        assertEquals(1, image.getPlacement(0).mColumns);
        assertNull(image.getPlacement(9));

        assertTrue(store.unplace(image, 5));
        assertNull(image.getPlacement(5));
        assertEquals(3, image.getPlacements().size());
        assertTrue(store.unplace(image, 0));
        assertFalse(image.hasPlacements());

        try {
            image.getPlacements().add(new ImageStore.VirtualPlacement(1, 1, 1));
            fail();
        } catch (UnsupportedOperationException e) {
            assertFalse(image.hasPlacements());
        }
    }

    public void testFindByNumberReturnsNewest() {
        ImageStore store = new ImageStore(100);
        store.put(1, 9, 1, 1, ImageStore.FORMAT_PNG, false, new byte[1]);
        store.put(2, 9, 1, 1, ImageStore.FORMAT_PNG, false, new byte[1]);
        store.put(3, 4, 1, 1, ImageStore.FORMAT_PNG, false, new byte[1]);
        assertEquals(2, store.findByNumber(9).mId);
        store.put(1, 9, 1, 1, ImageStore.FORMAT_PNG, false, new byte[1]);
        assertEquals(1, store.findByNumber(9).mId);
        assertNull(store.findByNumber(5));
        assertNull(store.findByNumber(0));
    }

}
