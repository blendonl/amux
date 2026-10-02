package com.termux.view;

import com.termux.terminal.ImageStore;

import java.util.Iterator;
import java.util.LinkedHashMap;
import java.util.Map;

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
    private final long mCapacity;
    private final Loader<B> mLoader;
    private ImageStore mStore;
    private int mModCount;
    private long mSize;
    private int mFrame;

    public BitmapCache(long capacity, Loader<B> loader) {
        mCapacity = capacity;
        mLoader = loader;
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
            if (store.get(image.mId) != image) {
                iterator.remove();
                release(entry.getValue());
            }
        }
    }

    public B get(ImageStore.Image image, int maxSide) {
        Entry<B> entry = mEntries.get(image);
        if (entry == null) {
            B bitmap = mLoader.load(image, maxSide);
            entry = new Entry<>(bitmap, bitmap == null ? 0 : mLoader.sizeOf(bitmap));
            mEntries.put(image, entry);
            mSize += entry.mSize;
            entry.mFrame = mFrame;
            trim();
        }
        entry.mFrame = mFrame;
        return entry.mBitmap;
    }

    public void clear() {
        for (Entry<B> entry : mEntries.values()) {
            if (entry.mBitmap != null) mLoader.release(entry.mBitmap);
        }
        mEntries.clear();
        mSize = 0;
        mStore = null;
    }

    private void trim() {
        Iterator<Entry<B>> iterator = mEntries.values().iterator();
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
