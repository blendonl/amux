package com.termux.terminal;

import java.util.ArrayList;
import java.util.Collection;
import java.util.Collections;
import java.util.HashMap;
import java.util.List;

public final class ImageStore {

    public static final long DEFAULT_CAPACITY = 48L * 1024 * 1024;

    public static final int FORMAT_RGB = 24;
    public static final int FORMAT_RGBA = 32;
    public static final int FORMAT_PNG = 100;

    public static final class VirtualPlacement {
        public final int mPlacementId;
        public final int mColumns;
        public final int mRows;

        VirtualPlacement(int placementId, int columns, int rows) {
            mPlacementId = placementId;
            mColumns = columns;
            mRows = rows;
        }
    }

    public static final class Image {
        public final int mId;
        public final int mNumber;
        public final int mWidth;
        public final int mHeight;
        public final int mFormat;
        public final boolean mCompressed;
        public final byte[] mPayload;
        public final int mGeneration;
        long mLastUsed;
        final List<VirtualPlacement> mPlacements = new ArrayList<>();

        Image(int id, int number, int width, int height, int format, boolean compressed, byte[] payload, int generation) {
            mId = id;
            mNumber = number;
            mWidth = width;
            mHeight = height;
            mFormat = format;
            mCompressed = compressed;
            mPayload = payload;
            mGeneration = generation;
        }

        public List<VirtualPlacement> getPlacements() {
            return Collections.unmodifiableList(mPlacements);
        }

        public VirtualPlacement getPlacement(int placementId) {
            for (VirtualPlacement placement : mPlacements) {
                if (placementId == 0 || placement.mPlacementId == placementId) return placement;
            }
            return null;
        }

        public boolean hasPlacements() {
            return !mPlacements.isEmpty();
        }
    }

    private final HashMap<Integer, Image> mImages = new HashMap<>();
    private final long mCapacity;
    private long mSize;
    private int mModCount;
    private int mGeneration;
    private long mClock;

    public ImageStore() {
        this(DEFAULT_CAPACITY);
    }

    public ImageStore(long capacity) {
        mCapacity = capacity;
    }

    public long getCapacity() {
        return mCapacity;
    }

    public long getSize() {
        return mSize;
    }

    public int getModCount() {
        return mModCount;
    }

    public int size() {
        return mImages.size();
    }

    public Image get(int id) {
        return mImages.get(id);
    }

    public Collection<Image> getImages() {
        return Collections.unmodifiableCollection(mImages.values());
    }

    public Image findByNumber(int number) {
        if (number == 0) return null;
        Image newest = null;
        for (Image image : mImages.values()) {
            if (image.mNumber == number && (newest == null || image.mGeneration > newest.mGeneration)) newest = image;
        }
        return newest;
    }

    public void touch(Image image) {
        image.mLastUsed = ++mClock;
    }

    Image put(int id, int number, int width, int height, int format, boolean compressed, byte[] payload) {
        if (payload.length > mCapacity) throw new IllegalArgumentException("Payload of " + payload.length + " bytes exceeds capacity " + mCapacity);
        Image previous = mImages.get(id);
        if (previous != null) drop(previous);
        Image image = new Image(id, number, width, height, format, compressed, payload, ++mGeneration);
        touch(image);
        mImages.put(id, image);
        mSize += payload.length;
        evictExcept(image);
        mModCount++;
        return image;
    }

    void place(Image image, int placementId, int columns, int rows) {
        VirtualPlacement placement = new VirtualPlacement(placementId, columns, rows);
        int index = placementId == 0 ? -1 : indexOfPlacement(image, placementId);
        if (index < 0) image.mPlacements.add(placement);
        else image.mPlacements.set(index, placement);
        touch(image);
        mModCount++;
    }

    private static int indexOfPlacement(Image image, int placementId) {
        for (int i = 0; i < image.mPlacements.size(); i++) {
            if (image.mPlacements.get(i).mPlacementId == placementId) return i;
        }
        return -1;
    }

    boolean unplace(Image image, int placementId) {
        boolean removed;
        if (placementId == 0) {
            removed = image.hasPlacements();
            image.mPlacements.clear();
        } else {
            removed = image.mPlacements.removeIf(placement -> placement.mPlacementId == placementId);
        }
        if (removed) mModCount++;
        return removed;
    }

    boolean remove(int id) {
        Image image = mImages.get(id);
        if (image == null) return false;
        drop(image);
        mModCount++;
        return true;
    }

    void clear() {
        if (mImages.isEmpty()) return;
        mImages.clear();
        mSize = 0;
        mModCount++;
    }

    private void evictExcept(Image keep) {
        while (mSize > mCapacity) {
            Image oldest = null;
            for (Image image : mImages.values()) {
                if (image != keep && (oldest == null || image.mLastUsed < oldest.mLastUsed)) oldest = image;
            }
            if (oldest == null) return;
            drop(oldest);
        }
    }

    private void drop(Image image) {
        mImages.remove(image.mId);
        mSize -= image.mPayload.length;
    }

}
