package com.termux.view;

import com.termux.terminal.ImageStore;

import java.util.HashMap;
import java.util.HashSet;
import java.util.Iterator;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.Executor;

public final class BitmapCache<B> {

    public static final long DEFAULT_CAPACITY = 128L * 1024 * 1024;

    public interface Loader<B> {
        B load(ImageStore.Image image, int maxSide);

        long sizeOf(B bitmap);

        void release(B bitmap);
    }

    private static final class Entry<B> {
        final B mBitmap;
        final long mSize;
        int mFrame;

        Entry(B bitmap, long size) {
            mBitmap = bitmap;
            mSize = size;
        }
    }

    private final LinkedHashMap<ImageStore.Image, Entry<B>> mEntries = new LinkedHashMap<>(16, 0.75f, true);
    private final HashMap<Integer, Entry<B>> mReplaced = new HashMap<>();
    private final Set<ImageStore.Image> mDecoding = new HashSet<>();
    private final long mCapacity;
    private final Loader<B> mLoader;
    private final Executor mDecoder;
    private final Executor mMainThread;
    private final Runnable mOnDecoded;
    private ImageStore mStore;
    private int mModCount;
    private long mSize;
    private int mFrame;

    public BitmapCache(long capacity, Loader<B> loader, Executor decoder, Executor mainThread, Runnable onDecoded) {
        mCapacity = capacity;
        mLoader = loader;
        mDecoder = decoder;
        mMainThread = mainThread;
        mOnDecoded = onDecoded;
    }

    public long getCapacity() {
        return mCapacity;
    }

    public long getSize() {
        return mSize;
    }

    public int size() {
        return mEntries.size();
    }

    public boolean contains(ImageStore.Image image) {
        return mEntries.containsKey(image);
    }

    public void sync(ImageStore store) {
        mFrame++;
        if (store != mStore) {
            clear();
            mStore = store;
            mModCount = store.getModCount();
            return;
        }
        if (store.getModCount() == mModCount) return;
        mModCount = store.getModCount();
        Iterator<Map.Entry<ImageStore.Image, Entry<B>>> iterator = mEntries.entrySet().iterator();
        while (iterator.hasNext()) {
            Map.Entry<ImageStore.Image, Entry<B>> entry = iterator.next();
            ImageStore.Image image = entry.getKey();
            if (store.get(image.mId) == image) continue;
            iterator.remove();
            if (store.get(image.mId) == null) release(entry.getValue());
            else replace(image.mId, entry.getValue());
        }
        Iterator<Map.Entry<Integer, Entry<B>>> replaced = mReplaced.entrySet().iterator();
        while (replaced.hasNext()) {
            Map.Entry<Integer, Entry<B>> entry = replaced.next();
            if (store.get(entry.getKey()) != null) continue;
            replaced.remove();
            release(entry.getValue());
        }
    }

    public B get(ImageStore.Image image, int maxSide) {
        Entry<B> entry = mEntries.get(image);
        if (entry == null) {
            decode(image, maxSide);
            entry = mEntries.get(image);
            if (entry == null) entry = mReplaced.get(image.mId);
            if (entry == null) return null;
        }
        entry.mFrame = mFrame;
        return entry.mBitmap;
    }

    public void clear() {
        for (Entry<B> entry : mEntries.values()) {
            if (entry.mBitmap != null) mLoader.release(entry.mBitmap);
        }
        for (Entry<B> entry : mReplaced.values()) {
            if (entry.mBitmap != null) mLoader.release(entry.mBitmap);
        }
        mEntries.clear();
        mReplaced.clear();
        mDecoding.clear();
        mSize = 0;
        mStore = null;
    }

    private void decode(ImageStore.Image image, int maxSide) {
        if (!mDecoding.add(image)) return;
        final ImageStore store = mStore;
        mDecoder.execute(() -> {
            final B bitmap = mLoader.load(image, maxSide);
            mMainThread.execute(() -> decoded(store, image, bitmap));
        });
    }

    private void decoded(ImageStore store, ImageStore.Image image, B bitmap) {
        mDecoding.remove(image);
        if (store == null || store != mStore || store.get(image.mId) != image || mEntries.containsKey(image)) {
            if (bitmap != null) mLoader.release(bitmap);
            return;
        }
        final Entry<B> replaced = mReplaced.remove(image.mId);
        if (replaced != null) release(replaced);
        final Entry<B> entry = new Entry<>(bitmap, bitmap == null ? 0 : mLoader.sizeOf(bitmap));
        entry.mFrame = mFrame;
        mEntries.put(image, entry);
        mSize += entry.mSize;
        trim();
        mOnDecoded.run();
    }

    private void replace(int id, Entry<B> entry) {
        final Entry<B> older = mReplaced.put(id, entry);
        if (older != null) release(older);
    }

    private void trim() {
        trim(mReplaced.values().iterator());
        trim(mEntries.values().iterator());
    }

    private void trim(Iterator<Entry<B>> iterator) {
        while (mSize > mCapacity && iterator.hasNext()) {
            Entry<B> entry = iterator.next();
            if (entry.mFrame == mFrame) continue;
            iterator.remove();
            release(entry);
        }
    }

    private void release(Entry<B> entry) {
        mSize -= entry.mSize;
        if (entry.mBitmap != null) mLoader.release(entry.mBitmap);
    }

}
