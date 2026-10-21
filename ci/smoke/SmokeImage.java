// スモークのテスト用プログラム（Smoke、SmokeSdl）が共有する補助：スクリーンショット、描くフレーム、
// F3+C の周りの Minecraft のデバッグのキー、カーソルを放す画面、開いているワールドのファイル。
// Java 8 のソース。

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

    // 今のコンテキストのバックバッファを PNG で保存する（調べるため：Wine＋llvmpipe のようにコピーする
    // ドライバでは、スワップの後も見せたフレームが残っている）。
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

    // 最小の PNG のエンコーダ（8 ビットの RGB、フィルタなし）。テスト用プログラムに java.desktop が
    // 要らないように。
    private static void writePng(String path, int width, int height, ByteBuffer rgb) throws IOException {
        ByteArrayOutputStream raw = new ByteArrayOutputStream();
        byte[] row = new byte[width * 3];
        for (int y = height - 1; y >= 0; y--) { // GL の行は下から上、PNG の行は上から下
            raw.write(0); // フィルタの種類：なし
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

    // フレームの中央の周りの模様（シザーでのクリアはどのプロファイルでも動く）。ズームしたフレームが
    // ズームしていないものと違って見えるように。
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

    // --sodium：Sodium を入れた Minecraft が設定する viewport。Sodium の GlStateManagerMixin は
    // 直前の呼び出しと同じ値で glViewport を呼ばず、Minecraft のライトマップの描画は 1 tick に 1 回
    // 16x16 の viewport を設定する（ここでは 13 フレームごと）。なので、ほとんどのフレームは viewport を
    // まったく設定せず、エージェントの高精細のズームはそれに対応しなければならない。
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

    // Minecraft のフレーム：場面を、ウィンドウシステムが報告するフレームバッファの大きさで自前の
    // フレームバッファ（"main render target"）に描き、それからウィンドウ（フレームバッファ 0）に
    // 写す。Minecraft 1.21.5 以降と同じ。GL 3.0 が要る。
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

    // 視線の方向から決まった角度にある四角形を、Minecraft の既定と同じく縦の視野角 70 度で描く：
    // ピクセルでの大きさはフレームの高さだけで大きくなるので、縦長のフレームでは大きく細かく見える。
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
        // 横の倍率も確かめるための、左右の青い印。
        for (int side = -1; side <= 1; side += 2) {
            int x = cx + (int) Math.round(side * 0.08 * pixelsPerTan);
            int half = (int) Math.max(1, Math.round(0.01 * pixelsPerTan));
            glScissor(x - half, cy - half, 2 * half, 2 * half);
            glClearColor(0.2f, 0.3f, 1f, 1f);
            glClear(GL_COLOR_BUFFER_BIT);
        }
        glDisable(GL_SCISSOR_TEST);
    }

    // F3+C に関わるところまでの Minecraft のデバッグのキー。この部分は 1.16.1、1.21.11、26.3 で同じ。
    // 修飾キー（F3）を押しているかはテスト用プログラムがその版と同じく決め、ゲームのウィンドウの
    // キーのイベントをすべて入れる：
    // - 修飾キーを押している間に押すかリピートしたキーはデバッグのキー。C は位置をコピーする
    //   （"F3C copied"）。ただし、デバッグ情報を制限したワールドのように拒否されたときは除く
    //   （"F3C refused"：ゲームはそのとき C をふつうのキーの押下として処理する）
    // - 修飾キーを離すと、押している間にデバッグのキーを処理していなければデバッグ画面を切り替える。
    //   なので拒否された F3+C は切り替える
    // - 修飾キーを押している間にクラッシュのキー（C）を物理的に押していると、デバッグのクラッシュが
    //   動き始め（"F3C crash armed"。テスト用プログラムは落ちない）、処理したものとして数える。
    //   動き始めて 100 ms たつとデバッグのキーは何もしないので、押したままの F3+C は 1 回だけコピーする
    static final class DebugKeys {
        // Minecraft が F3+C でコピーするもの（1.13 以降の形式）。エージェントの F3+C の横取りを試せるように。
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

        // どのキーのイベントでも最初に。
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

        // 修飾キーを押している間に押すかリピートしたキー。
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

    // インベントリのように 1 つのキーで開け閉めする Minecraft の画面（--screen-key）。エージェントの
    // キーの状態の偽装を試せるように：
    // - 開くとカーソルを放す（"SCREEN open"）
    // - 閉じるとカーソルをまた捕まえる（MouseHandler.grabMouse）。その前に、キーの割り当てのキーを
    //   ウィンドウシステムから読み直す（KeyMapping.setAll）。テスト用プログラムは、見ているキー
    //   （--watch-keys）を同じように読んで出す："SETALL <code>=<0|1> ..."
    // テスト用プログラムは、ゲームのウィンドウのキーの押下（リピートでないもの）を入れる。
    static final class Screen {
        private final int key;
        private final int[] watchKeys;
        private final IntPredicate keyDown;
        private final Runnable release, grab;
        private boolean open;

        // open：ゲームが画面を開いて始まるか（カーソルを捕まえていない）。
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

    // --watch-keys=87,341 のようなオプションのキーコード（GLFW のキーコードか SDL のスキャンコード）。
    // それぞれ min から max の間。
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

    // --screen-key=69 のようなオプションの 1 つのキーコード。
    static int keyCode(String arg, int min, int max) {
        int[] codes = keyCodes(arg, min, max);
        if (codes.length != 1) {
            throw new IllegalArgumentException(arg.substring(0, arg.indexOf('=')) + " needs one key code");
        }
        return codes[0];
    }

    // シングルプレイのワールドを開いている間に Minecraft がゲームのフォルダに残すもの。エージェントは
    // これでワールドを知る：saves/<name>/session.lock（最も新しく書いたものが開いているワールド）と、
    // logs/latest.log の統合サーバーの開始の行。今のフォルダに書く。gamedir= のオプションで指定しない
    // 限り、ここがエージェントのゲームのフォルダ。
    static void writeWorld(String name) {
        if (name.isEmpty() || name.equals(".") || name.equals("..") || name.contains("/") || name.contains("\\")) {
            throw new IllegalArgumentException("--world needs a folder name, not '" + name + "'");
        }
        try {
            Path lock = Paths.get("saves", name, "session.lock");
            Files.createDirectories(lock.getParent());
            Files.write(lock, "☃".getBytes(StandardCharsets.UTF_8)); // ゲームは雪だるまを書く
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
