// Screenshot helper shared by the smoke harnesses (Smoke, SmokeSdl). Java 8 source.

import java.io.ByteArrayOutputStream;
import java.io.DataOutputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.UncheckedIOException;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.zip.CRC32;
import java.util.zip.DeflaterOutputStream;

import org.lwjgl.BufferUtils;

import static org.lwjgl.opengl.GL11.*;

final class SmokeImage {
    private SmokeImage() {}

    // Saves the current context's back buffer as a PNG (diagnostic: after a swap it still
    // holds the presented frame on drivers that copy, as Wine + llvmpipe does).
    static void saveBackBuffer(int width, int height, String path) {
        glFinish();
        glReadBuffer(GL_BACK);
        glPixelStorei(GL_PACK_ALIGNMENT, 1);
        ByteBuffer rgb = BufferUtils.createByteBuffer(width * height * 3);
        glReadPixels(0, 0, width, height, GL_RGB, GL_UNSIGNED_BYTE, rgb);
        try {
            writePng(path, width, height, rgb);
        } catch (IOException e) {
            throw new UncheckedIOException("screenshot " + path, e);
        }
        System.out.println("SCREENSHOT " + path);
    }

    // Minimal PNG encoder (8-bit RGB, no filtering), so the harness needs no java.desktop.
    private static void writePng(String path, int width, int height, ByteBuffer rgb) throws IOException {
        ByteArrayOutputStream raw = new ByteArrayOutputStream();
        byte[] row = new byte[width * 3];
        for (int y = height - 1; y >= 0; y--) { // GL rows go bottom-up, PNG rows top-down
            raw.write(0); // filter type: none
            rgb.position(y * width * 3);
            rgb.get(row);
            raw.write(row);
        }
        ByteArrayOutputStream compressed = new ByteArrayOutputStream();
        try (DeflaterOutputStream deflater = new DeflaterOutputStream(compressed)) {
            raw.writeTo(deflater);
        }
        ByteBuffer header = ByteBuffer.allocate(13);
        header.putInt(width).putInt(height).put((byte) 8).put((byte) 2).put((byte) 0).put((byte) 0).put((byte) 0);

        ByteArrayOutputStream png = new ByteArrayOutputStream();
        png.write(new byte[] {(byte) 0x89, 'P', 'N', 'G', '\r', '\n', 0x1A, '\n'});
        chunk(png, "IHDR", header.array());
        chunk(png, "IDAT", compressed.toByteArray());
        chunk(png, "IEND", new byte[0]);
        try (FileOutputStream out = new FileOutputStream(path)) {
            png.writeTo(out);
        }
    }

    private static void chunk(ByteArrayOutputStream png, String type, byte[] data) throws IOException {
        DataOutputStream out = new DataOutputStream(png);
        byte[] typeBytes = type.getBytes(StandardCharsets.US_ASCII);
        CRC32 crc = new CRC32();
        crc.update(typeBytes);
        crc.update(data);
        out.writeInt(data.length);
        out.write(typeBytes);
        out.write(data);
        out.writeInt((int) crc.getValue());
    }
}
