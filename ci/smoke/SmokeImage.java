// Helpers shared by the smoke harnesses (Smoke, SmokeSdl): screenshots, the drawn frames,
// Minecraft's debug keys around F3+C, a screen that releases the cursor, and the files of an
// open world. Java 8 source.

import java.io.ByteArrayOutputStream;
import java.io.DataOutputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.UncheckedIOException;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.function.IntPredicate;
import java.util.zip.CRC32;
import java.util.zip.DeflaterOutputStream;

import org.lwjgl.BufferUtils;

import org.lwjgl.opengl.GL30;

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

    // A pattern around the centre of the frame (scissored clears work in any profile), so
    // a zoomed frame looks different from an unzoomed one.
    static void drawPattern(int width, int height) {
        glEnable(GL_SCISSOR_TEST);
        int cx = width / 2, cy = height / 2;
        float[][] colors = {{0.9f, 0.2f, 0.2f}, {0.2f, 0.8f, 0.2f}, {0.9f, 0.9f, 0.2f}, {0.9f, 0.5f, 0.1f}};
        int[][] offsets = {{-40, 0}, {0, 0}, {-40, -40}, {0, -40}};
        for (int i = 0; i < 4; i++) {
            glScissor(cx + offsets[i][0], cy + offsets[i][1], 40, 40);
            glClearColor(colors[i][0], colors[i][1], colors[i][2], 1f);
            glClear(GL_COLOR_BUFFER_BIT);
        }
        glDisable(GL_SCISSOR_TEST);
    }

    // --sodium: the viewport as Minecraft with Sodium sets it. Sodium's GlStateManagerMixin
    // does not call glViewport with the values of the last call, and Minecraft's lightmap pass
    // sets a 16x16 viewport once a tick (here every 13th frame). So most frames set no
    // viewport at all, which the agent's high-resolution zoom must cope with.
    static boolean sodium;
    private static int lastViewportX = -1, lastViewportY, lastViewportW, lastViewportH;

    static void viewport(int x, int y, int w, int h) {
        if (sodium && x == lastViewportX && y == lastViewportY && w == lastViewportW && h == lastViewportH) {
            return;
        }
        lastViewportX = x;
        lastViewportY = y;
        lastViewportW = w;
        lastViewportH = h;
        glViewport(x, y, w, h);
    }

    // Minecraft's frame: the scene goes into its own framebuffer ("main render target") at
    // the framebuffer size the window system reports, and is then copied into the window
    // (framebuffer 0), as Minecraft 1.21.5+ does. Needs GL 3.0.
    static final class MainTarget {
        private int framebuffer, texture, width, height, frame;

        void render(int w, int h) {
            if (framebuffer == 0 || w != width || h != height) {
                resize(w, h);
            }
            if (sodium && ++frame % 13 == 0) {
                viewport(0, 0, 16, 16);
            }
            GL30.glBindFramebuffer(GL30.GL_FRAMEBUFFER, framebuffer);
            viewport(0, 0, width, height);
            glClearColor(0.2f, 0.4f, 0.6f, 1f);
            glClear(GL_COLOR_BUFFER_BIT);
            drawScene(width, height);
        }

        void present() {
            GL30.glBindFramebuffer(GL30.GL_READ_FRAMEBUFFER, framebuffer);
            GL30.glBindFramebuffer(GL30.GL_DRAW_FRAMEBUFFER, 0);
            viewport(0, 0, width, height);
            GL30.glBlitFramebuffer(0, 0, width, height, 0, 0, width, height, GL_COLOR_BUFFER_BIT, GL_NEAREST);
            GL30.glBindFramebuffer(GL30.GL_FRAMEBUFFER, 0);
        }

        private void resize(int w, int h) {
            if (framebuffer != 0) {
                GL30.glDeleteFramebuffers(framebuffer);
                glDeleteTextures(texture);
            }
            width = Math.max(1, w);
            height = Math.max(1, h);
            texture = glGenTextures();
            glBindTexture(GL_TEXTURE_2D, texture);
            glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA8, width, height, 0, GL_RGBA, GL_UNSIGNED_BYTE, (ByteBuffer) null);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
            glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
            glBindTexture(GL_TEXTURE_2D, 0);
            framebuffer = GL30.glGenFramebuffers();
            GL30.glBindFramebuffer(GL30.GL_FRAMEBUFFER, framebuffer);
            GL30.glFramebufferTexture2D(GL30.GL_FRAMEBUFFER, GL30.GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, texture, 0);
            System.out.println("RESIZE " + width + "x" + height);
        }
    }

    // Squares at fixed angles from the view direction, drawn with a vertical field of view
    // of 70 degrees like Minecraft's default: their size in pixels grows with the frame's
    // height only, so a taller frame shows them larger and in more detail.
    static void drawScene(int width, int height) {
        double pixelsPerTan = height / 2.0 / Math.tan(Math.toRadians(35));
        int cx = width / 2, cy = height / 2;
        double[] sizes = {0.3, 0.12, 0.04, 0.012, 0.004};
        float[][] colors = {{0.1f, 0.5f, 0.1f}, {0.9f, 0.9f, 0.2f}, {0.9f, 0.2f, 0.2f}, {1f, 1f, 1f}, {0f, 0f, 0f}};
        glEnable(GL_SCISSOR_TEST);
        for (int i = 0; i < sizes.length; i++) {
            int half = (int) Math.max(1, Math.round(sizes[i] * pixelsPerTan));
            glScissor(cx - half, cy - half, 2 * half, 2 * half);
            glClearColor(colors[i][0], colors[i][1], colors[i][2], 1f);
            glClear(GL_COLOR_BUFFER_BIT);
        }
        // Blue markers left and right, to check the horizontal scale too.
        for (int side = -1; side <= 1; side += 2) {
            int x = cx + (int) Math.round(side * 0.08 * pixelsPerTan);
            int half = (int) Math.max(1, Math.round(0.01 * pixelsPerTan));
            glScissor(x - half, cy - half, 2 * half, 2 * half);
            glClearColor(0.2f, 0.3f, 1f, 1f);
            glClear(GL_COLOR_BUFFER_BIT);
        }
        glDisable(GL_SCISSOR_TEST);
    }

    // Minecraft's debug keys as far as F3+C goes; 1.16.1, 1.21.11 and 26.3 agree on this part.
    // The harness decides whether the modifier (F3) is held, as its version does, and feeds
    // every key event of the game's window in:
    // - a key pressed or repeated while the modifier is held is a debug key; C copies the
    //   location ("F3C copied"), unless refused like in a world with reduced debug info
    //   ("F3C refused": the game then handles C as a normal key press);
    // - releasing the modifier toggles the debug overlay unless a debug key was handled while
    //   it was held, so a refused F3+C toggles it;
    // - the crash key (C) physically down while the modifier is held arms the debug crash
    //   ("F3C crash armed"; the harness never crashes), which counts as handled; once armed for
    //   100 ms, debug keys do nothing, so a held F3+C copies once.
    static final class DebugKeys {
        // What Minecraft copies on F3+C (format of 1.13+), so the agent's F3+C capture can be tested.
        static final String F3C_TEXT =
            "/execute in minecraft:overworld run tp @s 12.50 64.00 -7.25 90.00 15.00";

        private final boolean refuse;
        private final Runnable copy;
        private boolean overlay, handled, crashArmed;
        private long crashArmedAt;
        private int copies;

        DebugKeys(boolean refuse, Runnable copy) {
            this.refuse = refuse;
            this.copy = copy;
        }

        // First thing for every key event.
        void keyEvent(boolean crashKeyDown, boolean modifierHeld) {
            if (!crashKeyDown || !modifierHeld) {
                crashArmed = false;
                return;
            }
            if (!crashArmed) {
                crashArmed = true;
                crashArmedAt = System.nanoTime();
                System.out.println("F3C crash armed");
            }
            handled = true;
        }

        // A key pressed or repeated while the modifier is held.
        void debugKey(boolean copyKey) {
            if (crashArmed && System.nanoTime() - crashArmedAt > 100_000_000L) {
                handled = true;
            } else if (copyKey && refuse) {
                System.out.println("F3C refused");
            } else if (copyKey) {
                copy.run();
                copies++;
                handled = true;
                System.out.println("F3C copied");
            }
        }

        void modifierReleased() {
            if (handled) {
                handled = false;
                return;
            }
            overlay = !overlay;
            System.out.println("F3C overlay " + (overlay ? "on" : "off"));
        }

        void printState(boolean modifierDown) {
            System.out.printf("F3C STATE overlay=%s modifier=%s copies=%d%n",
                overlay ? "on" : "off", modifierDown ? "down" : "up", copies);
        }
    }

    // A Minecraft screen opened and closed with one key (--screen-key), as the inventory is,
    // so the agent's key state spoofing can be tested:
    // - opening it releases the cursor ("SCREEN open");
    // - closing it grabs the cursor again (MouseHandler.grabMouse), which first re-reads the
    //   keyboard mappings' keys from the window system (KeyMapping.setAll). The harness prints
    //   the watched keys (--watch-keys) as read the same way: "SETALL <code>=<0|1> ...".
    // The harness feeds in the key presses (not repeats) of the game's window.
    static final class Screen {
        private final int key;
        private final int[] watchKeys;
        private final IntPredicate keyDown;
        private final Runnable release, grab;
        private boolean open;

        // open: whether the game starts on a screen (the cursor is not grabbed).
        Screen(int key, boolean open, int[] watchKeys, IntPredicate keyDown, Runnable release, Runnable grab) {
            this.key = key;
            this.open = open;
            this.watchKeys = watchKeys;
            this.keyDown = keyDown;
            this.release = release;
            this.grab = grab;
        }

        void keyPressed(int code) {
            if (code != key) {
                return;
            }
            if (!open) {
                open = true;
                System.out.println("SCREEN open");
                release.run();
                return;
            }
            StringBuilder line = new StringBuilder("SETALL");
            for (int watched : watchKeys) {
                line.append(' ').append(watched).append('=').append(keyDown.test(watched) ? 1 : 0);
            }
            System.out.println(line);
            open = false;
            grab.run();
        }
    }

    // The key codes of an option like --watch-keys=87,341 (GLFW key codes or SDL scancodes),
    // each from min to max.
    static int[] keyCodes(String arg, int min, int max) {
        int eq = arg.indexOf('=');
        String[] parts = arg.substring(eq + 1).split(",", -1);
        int[] codes = new int[parts.length];
        for (int i = 0; i < parts.length; i++) {
            int code;
            try {
                code = Integer.parseInt(parts[i].trim());
            } catch (NumberFormatException e) {
                code = min - 1;
            }
            if (code < min || code > max) {
                throw new IllegalArgumentException(arg.substring(0, eq) + " needs key codes from " + min
                    + " to " + max + ", not '" + parts[i] + "'");
            }
            codes[i] = code;
        }
        return codes;
    }

    // The one key code of an option like --screen-key=69.
    static int keyCode(String arg, int min, int max) {
        int[] codes = keyCodes(arg, min, max);
        if (codes.length != 1) {
            throw new IllegalArgumentException(arg.substring(0, arg.indexOf('=')) + " needs one key code");
        }
        return codes[0];
    }

    // What Minecraft leaves in its game directory while a singleplayer world is open, which is
    // how the agent tells the world: saves/<name>/session.lock (the most recently written one is
    // the open world) and the integrated server's start line in logs/latest.log. Written into
    // the current directory, the agent's game directory unless its gamedir= option says otherwise.
    static void writeWorld(String name) {
        if (name.isEmpty() || name.equals(".") || name.equals("..") || name.contains("/") || name.contains("\\")) {
            throw new IllegalArgumentException("--world needs a folder name, not '" + name + "'");
        }
        try {
            Path lock = Paths.get("saves", name, "session.lock");
            Files.createDirectories(lock.getParent());
            Files.write(lock, "☃".getBytes(StandardCharsets.UTF_8)); // the game writes a snowman
            Path log = Paths.get("logs", "latest.log");
            Files.createDirectories(log.getParent());
            String line = "[00:00:00] [Server thread/INFO]: Starting integrated minecraft server version smoke\n";
            Files.write(log, line.getBytes(StandardCharsets.UTF_8));
        } catch (IOException e) {
            throw new UncheckedIOException("--world=" + name, e);
        }
        System.out.println("WORLD " + name);
    }
}
