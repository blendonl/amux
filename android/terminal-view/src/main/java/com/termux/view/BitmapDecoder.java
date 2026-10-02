package com.termux.view;

import android.graphics.Bitmap;
import android.graphics.BitmapFactory;

import com.termux.terminal.ImageStore;

import java.io.IOException;
import java.io.InputStream;

final class BitmapDecoder implements BitmapCache.Loader<Bitmap> {

    static final int MAX_SIDE = 4096;

    @Override
    public Bitmap load(ImageStore.Image image, int maxSide) {
        try {
            return image.mFormat == ImageStore.FORMAT_PNG ? decodePng(image, maxSide) : decodeRaw(image, maxSide);
        } catch (IOException | RuntimeException | OutOfMemoryError e) {
            return null;
        }
    }

    @Override
    public long sizeOf(Bitmap bitmap) {
        return bitmap.getAllocationByteCount();
    }

    @Override
    public void release(Bitmap bitmap) {
        bitmap.recycle();
    }

    private static Bitmap decodePng(ImageStore.Image image, int maxSide) throws IOException {
        BitmapFactory.Options options = new BitmapFactory.Options();
        options.inSampleSize = Pixels.sampleStep(image.mWidth, image.mHeight, maxSide);
        Bitmap bitmap;
        try (InputStream in = Pixels.open(image.mPayload, image.mCompressed)) {
            bitmap = BitmapFactory.decodeStream(in, null, options);
        }
        if (bitmap == null || bitmap.getConfig() == Bitmap.Config.ARGB_8888) return bitmap;
        Bitmap converted = bitmap.copy(Bitmap.Config.ARGB_8888, false);
        bitmap.recycle();
        return converted;
    }

    private static Bitmap decodeRaw(ImageStore.Image image, int maxSide) throws IOException {
        final int step = Pixels.sampleStep(image.mWidth, image.mHeight, maxSide);
        final int width = Pixels.scaled(image.mWidth, step);
        final Bitmap bitmap = Bitmap.createBitmap(width, Pixels.scaled(image.mHeight, step), Bitmap.Config.ARGB_8888);
        bitmap.setHasAlpha(image.mFormat == ImageStore.FORMAT_RGBA);
        try {
            Pixels.decode(image.mPayload, image.mCompressed, image.mWidth, image.mHeight, image.mFormat, step,
                (row, argb) -> bitmap.setPixels(argb, 0, width, 0, row, width, 1));
        } catch (IOException | RuntimeException e) {
            bitmap.recycle();
            throw e;
        }
        return bitmap;
    }

}
