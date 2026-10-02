package com.termux.view;

import com.termux.terminal.ImageStore;
import com.termux.terminal.TerminalEmulator;
import com.termux.terminal.TerminalOutput;

import junit.framework.TestCase;

import java.nio.charset.StandardCharsets;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Base64;
import java.util.List;
import java.util.concurrent.Executor;

public class BitmapCacheTest extends TestCase {

    private static final long BITMAP_SIZE = 100;

    private static final class NullOutput extends TerminalOutput {
        @Override
        public void write(byte[] data, int offset, int count) {
        }

        @Override
        public void titleChanged(String oldTitle, String newTitle) {
        }

        @Override
        public void onCopyTextToClipboard(String text) {
        }

        @Override
        public void onPasteTextFromClipboard() {
        }

        @Override
        public void onBell() {
        }

        @Override
        public void onColorsChanged() {
        }
    }

    private static final class FakeBitmap {
        final ImageStore.Image mImage;
        final int mMaxSide;
        boolean mReleased;

        FakeBitmap(ImageStore.Image image, int maxSide) {
            mImage = image;
            mMaxSide = maxSide;
        }
    }

    private static final class FakeLoader implements BitmapCache.Loader<FakeBitmap> {
        final List<FakeBitmap> mLoaded = new ArrayList<>();
        int mCalls;
        boolean mFail;

        @Override
        public FakeBitmap load(ImageStore.Image image, int maxSide) {
            mCalls++;
            if (mFail) return null;
            FakeBitmap bitmap = new FakeBitmap(image, maxSide);
            mLoaded.add(bitmap);
            return bitmap;
        }

        @Override
        public long sizeOf(FakeBitmap bitmap) {
            return BITMAP_SIZE;
        }

        @Override
        public void release(FakeBitmap bitmap) {
            assertFalse("released twice", bitmap.mReleased);
            bitmap.mReleased = true;
        }
    }

    private static final class QueuedExecutor implements Executor {
        final ArrayDeque<Runnable> mQueued = new ArrayDeque<>();

        @Override
        public void execute(Runnable command) {
            mQueued.add(command);
        }

        void runAll() {
            while (!mQueued.isEmpty()) mQueued.poll().run();
        }
    }

    private static final Executor DIRECT = Runnable::run;

    private final FakeLoader mLoader = new FakeLoader();
    private final QueuedExecutor mDecoder = new QueuedExecutor();
    private int mDecoded;
    private TerminalEmulator mEmulator;
    private ImageStore mStore;

    @Override
    protected void setUp() throws Exception {
        super.setUp();
        mEmulator = newEmulator();
        mStore = mEmulator.getImages();
    }

    private static TerminalEmulator newEmulator() {
        return new TerminalEmulator(new NullOutput(), 10, 4, 10, 20, 8, null);
    }

    private static void enter(TerminalEmulator emulator, String text) {
        byte[] bytes = text.getBytes(StandardCharsets.UTF_8);
        emulator.append(bytes, bytes.length);
    }

    private static ImageStore.Image transmit(TerminalEmulator emulator, int id) {
        String pixel = Base64.getEncoder().encodeToString(new byte[]{1, 2, 3});
        enter(emulator, "\033_Ga=T,U=1,f=24,s=1,v=1,q=2,i=" + id + ";" + pixel + "\033\\");
        ImageStore.Image image = emulator.getImages().get(id);
        assertNotNull(image);
        return image;
    }

    private ImageStore.Image transmit(int id) {
        return transmit(mEmulator, id);
    }

    private void delete(int id) {
        enter(mEmulator, "\033_Ga=d,d=I,q=2,i=" + id + "\033\\");
    }

    private BitmapCache<FakeBitmap> newCache(long capacity) {
        return new BitmapCache<>(capacity, mLoader, DIRECT, DIRECT, () -> mDecoded++);
    }

    private BitmapCache<FakeBitmap> newQueuedCache() {
        return new BitmapCache<>(1000, mLoader, mDecoder, DIRECT, () -> mDecoded++);
    }

    public void testLoadsLazilyOnceAndReuses() {
        BitmapCache<FakeBitmap> cache = newCache(1000);
        ImageStore.Image image = transmit(1);
        cache.sync(mStore);
        assertEquals(0, mLoader.mCalls);

        FakeBitmap bitmap = cache.get(image, 4096);
        assertSame(image, bitmap.mImage);
        assertEquals(4096, bitmap.mMaxSide);
        cache.sync(mStore);
        assertSame(bitmap, cache.get(image, 4096));
        assertEquals(1, mLoader.mCalls);
        assertEquals(BITMAP_SIZE, cache.getSize());
        assertEquals(1, mDecoded);
    }

    public void testRetransmittedImageIsReloaded() {
        BitmapCache<FakeBitmap> cache = newCache(1000);
        ImageStore.Image first = transmit(1);
        cache.sync(mStore);
        FakeBitmap firstBitmap = cache.get(first, 4096);

        ImageStore.Image second = transmit(1);
        assertNotSame(first, second);
        assertEquals(first.mId, second.mId);
        assertTrue(second.mGeneration != first.mGeneration);
        cache.sync(mStore);
        assertFalse(cache.contains(first));
        assertFalse(firstBitmap.mReleased);

        FakeBitmap secondBitmap = cache.get(second, 4096);
        assertSame(second, secondBitmap.mImage);
        assertTrue(firstBitmap.mReleased);
        assertEquals(BITMAP_SIZE, cache.getSize());
        assertEquals(2, mLoader.mCalls);
    }

    public void testAMissDecodesInTheBackgroundOnce() {
        BitmapCache<FakeBitmap> cache = newQueuedCache();
        ImageStore.Image image = transmit(1);
        cache.sync(mStore);

        assertNull(cache.get(image, 4096));
        assertNull(cache.get(image, 4096));
        assertEquals(0, mLoader.mCalls);
        assertEquals(1, mDecoder.mQueued.size());

        mDecoder.runAll();
        assertEquals(1, mLoader.mCalls);
        assertEquals(1, mDecoded);
        assertSame(image, cache.get(image, 4096).mImage);
    }

    public void testThePreviousBitmapShowsUntilTheNewOneIsDecoded() {
        BitmapCache<FakeBitmap> cache = newQueuedCache();
        ImageStore.Image first = transmit(1);
        cache.sync(mStore);
        cache.get(first, 4096);
        mDecoder.runAll();
        FakeBitmap firstBitmap = cache.get(first, 4096);

        ImageStore.Image second = transmit(1);
        cache.sync(mStore);
        assertSame(firstBitmap, cache.get(second, 4096));
        assertEquals(BITMAP_SIZE, cache.getSize());

        mDecoder.runAll();
        assertTrue(firstBitmap.mReleased);
        assertSame(second, cache.get(second, 4096).mImage);
        assertEquals(BITMAP_SIZE, cache.getSize());
    }

    public void testAnImageReplacedDuringItsDecodeIsDropped() {
        BitmapCache<FakeBitmap> cache = newQueuedCache();
        ImageStore.Image first = transmit(1);
        cache.sync(mStore);
        cache.get(first, 4096);
        ImageStore.Image second = transmit(1);
        cache.sync(mStore);

        mDecoder.runAll();
        assertTrue(mLoader.mLoaded.get(0).mReleased);
        assertFalse(cache.contains(first));
        assertEquals(0, mDecoded);
        assertNull(cache.get(second, 4096));
    }

    public void testAnImageDeletedDuringItsDecodeIsDropped() {
        BitmapCache<FakeBitmap> cache = newQueuedCache();
        ImageStore.Image image = transmit(1);
        cache.sync(mStore);
        cache.get(image, 4096);
        delete(1);
        cache.sync(mStore);

        mDecoder.runAll();
        assertTrue(mLoader.mLoaded.get(0).mReleased);
        assertEquals(0, cache.size());
        assertEquals(0, cache.getSize());
    }

    public void testADecodeThatFinishesAfterClearIsDropped() {
        BitmapCache<FakeBitmap> cache = newQueuedCache();
        ImageStore.Image image = transmit(1);
        cache.sync(mStore);
        cache.get(image, 4096);
        cache.clear();

        mDecoder.runAll();
        assertTrue(mLoader.mLoaded.get(0).mReleased);
        assertEquals(0, cache.size());
        assertEquals(0, mDecoded);
    }

    public void testADeletedIdLetsGoOfItsPreviousBitmap() {
        BitmapCache<FakeBitmap> cache = newCache(1000);
        ImageStore.Image first = transmit(1);
        cache.sync(mStore);
        FakeBitmap firstBitmap = cache.get(first, 4096);
        transmit(1);
        cache.sync(mStore);
        assertFalse(firstBitmap.mReleased);

        delete(1);
        cache.sync(mStore);
        assertTrue(firstBitmap.mReleased);
        assertEquals(0, cache.getSize());
    }

    public void testDeletedImageIsReleasedOnNextSync() {
        BitmapCache<FakeBitmap> cache = newCache(1000);
        ImageStore.Image kept = transmit(1);
        ImageStore.Image deleted = transmit(2);
        cache.sync(mStore);
        FakeBitmap keptBitmap = cache.get(kept, 4096);
        FakeBitmap deletedBitmap = cache.get(deleted, 4096);

        delete(2);
        assertFalse(deletedBitmap.mReleased);
        cache.sync(mStore);
        assertTrue(deletedBitmap.mReleased);
        assertFalse(keptBitmap.mReleased);
        assertEquals(1, cache.size());
        assertEquals(BITMAP_SIZE, cache.getSize());
    }

    public void testSyncWithoutStoreChangesKeepsEverything() {
        BitmapCache<FakeBitmap> cache = newCache(1000);
        ImageStore.Image image = transmit(1);
        cache.sync(mStore);
        FakeBitmap bitmap = cache.get(image, 4096);
        for (int i = 0; i < 3; i++) cache.sync(mStore);
        assertFalse(bitmap.mReleased);
        assertEquals(1, cache.size());
    }

    public void testLeastRecentlyUsedIsEvicted() {
        BitmapCache<FakeBitmap> cache = newCache(250);
        ImageStore.Image a = transmit(1);
        ImageStore.Image b = transmit(2);
        ImageStore.Image c = transmit(3);
        cache.sync(mStore);
        FakeBitmap aBitmap = cache.get(a, 4096);
        cache.sync(mStore);
        FakeBitmap bBitmap = cache.get(b, 4096);
        cache.sync(mStore);
        cache.get(a, 4096);
        cache.sync(mStore);
        FakeBitmap cBitmap = cache.get(c, 4096);

        assertTrue(bBitmap.mReleased);
        assertFalse(aBitmap.mReleased);
        assertFalse(cBitmap.mReleased);
        assertEquals(2 * BITMAP_SIZE, cache.getSize());
        assertTrue(cache.getSize() <= cache.getCapacity());
    }

    public void testBitmapsDrawnThisFrameAreNotEvicted() {
        BitmapCache<FakeBitmap> cache = newCache(150);
        ImageStore.Image a = transmit(1);
        ImageStore.Image b = transmit(2);
        ImageStore.Image c = transmit(3);
        cache.sync(mStore);
        FakeBitmap aBitmap = cache.get(a, 4096);
        FakeBitmap bBitmap = cache.get(b, 4096);
        assertFalse(aBitmap.mReleased);
        assertFalse(bBitmap.mReleased);
        assertEquals(2 * BITMAP_SIZE, cache.getSize());

        cache.sync(mStore);
        cache.get(b, 4096);
        FakeBitmap cBitmap = cache.get(c, 4096);
        assertTrue(aBitmap.mReleased);
        assertFalse(bBitmap.mReleased);
        assertFalse(cBitmap.mReleased);
        assertEquals(2 * BITMAP_SIZE, cache.getSize());

        cache.sync(mStore);
        cache.get(a, 4096);
        assertTrue(bBitmap.mReleased);
        assertTrue(cBitmap.mReleased);
        assertEquals(BITMAP_SIZE, cache.getSize());
    }

    public void testFailedLoadIsNotRetried() {
        BitmapCache<FakeBitmap> cache = newCache(1000);
        ImageStore.Image image = transmit(1);
        mLoader.mFail = true;
        cache.sync(mStore);
        assertNull(cache.get(image, 4096));
        cache.sync(mStore);
        assertNull(cache.get(image, 4096));
        assertEquals(1, mLoader.mCalls);
        assertEquals(0, cache.getSize());

        mLoader.mFail = false;
        ImageStore.Image replaced = transmit(1);
        cache.sync(mStore);
        assertNotNull(cache.get(replaced, 4096));
    }

    public void testAnotherStoreClearsTheCache() {
        BitmapCache<FakeBitmap> cache = newCache(1000);
        ImageStore.Image image = transmit(1);
        cache.sync(mStore);
        FakeBitmap bitmap = cache.get(image, 4096);

        TerminalEmulator other = newEmulator();
        ImageStore.Image otherImage = transmit(other, 1);
        cache.sync(other.getImages());
        assertTrue(bitmap.mReleased);
        assertEquals(0, cache.size());
        assertNotNull(cache.get(otherImage, 4096));
    }

    public void testClearReleasesEverything() {
        BitmapCache<FakeBitmap> cache = newCache(1000);
        ImageStore.Image a = transmit(1);
        ImageStore.Image b = transmit(2);
        cache.sync(mStore);
        FakeBitmap aBitmap = cache.get(a, 4096);
        FakeBitmap bBitmap = cache.get(b, 4096);

        cache.clear();
        assertTrue(aBitmap.mReleased);
        assertTrue(bBitmap.mReleased);
        assertEquals(0, cache.size());
        assertEquals(0, cache.getSize());

        cache.sync(mStore);
        assertNotSame(aBitmap, cache.get(a, 4096));
    }

    public void testResetDropsEverythingOnSync() {
        BitmapCache<FakeBitmap> cache = newCache(1000);
        ImageStore.Image image = transmit(1);
        cache.sync(mStore);
        FakeBitmap bitmap = cache.get(image, 4096);
        enter(mEmulator, "\033c");
        cache.sync(mStore);
        assertTrue(bitmap.mReleased);
        assertEquals(0, cache.size());
    }

}
