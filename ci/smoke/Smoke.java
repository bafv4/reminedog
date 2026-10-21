// Minecraft の LWJGL3／GLFW のウィンドウとフレームのループの最小の代役。ゲームなしで
// reminedog のエージェント（-agentpath:）を試すのに使う。Java 8 のソース：javac --release 8。
//
// 使い方：java -cp <lwjgl jars>;<classes> Smoke [FRAMES] [--legacy] [--readback] [--screenshot]
//                                            [--seconds=N] [--capture] [--mc [--sodium]] [--world=NAME]
//                                            [--f3c-events] [--f3c-refuse]
//                                            [--screen-key=KEY [--watch-keys=KEY,...]]
//   FRAMES      描くフレームの数（既定は 120）
//   --legacy    Minecraft 1.13〜1.16 と同じく既定のウィンドウのヒント（なければ 1.17 以降と同じく 3.2 core）
//   --readback  最後のスワップの後、いくつかのウィンドウの座標について "PIXEL x y r g b"（フロント
//               バッファ）と "PIXEL_BACK x y r g b"（バックバッファ）を出す
//   --seconds=N   フレームの数の代わりに、約 60 fps で N 秒動かす（入力のテスト用）
//   --capture     ゲーム中の Minecraft と同じくカーソルを捕まえる（GLFW_CURSOR_DISABLED）。使えれば
//                 マウスの生の移動も（Minecraft の「Raw Input」の設定。既定でオン）
//   --mc          Minecraft と同じく描く：フレームバッファの大きさのコールバックが報告する大きさで
//                 自前のフレームバッファに描き、ウィンドウに写す
//   --sodium      --mc と一緒に、Sodium を入れた Minecraft と同じく viewport を設定する（直前と同じ
//                 glViewport を省く。SmokeImage.viewport）
//   --screenshot  最後のスワップの後、バックバッファを今のフォルダに smoke-screenshot.png として保存する
//                 （調べるため：オーバーレイが何を描いたか。Wine＋llvmpipe のように、スワップの後も
//                 ドライバがバックバッファを残すところで）
//   --world=NAME  始めに、シングルプレイのワールドを開いたときの Minecraft と同じく、
//                 saves/NAME/session.lock と、統合サーバーの開始の行を持つ logs/latest.log を今の
//                 フォルダに書く（エージェントがワールドを知るように）
//   --f3c-events  Minecraft 1.21.9 以降と同じく、キーのイベントで F3 を押しているかを決める
//                 （なければ 1.16 と同じく、キーを処理するときの glfwGetKey(F3) で）
//   --f3c-refuse  デバッグ情報を制限したワールドと同じく、F3+C で何もコピーしない
//   --screen-key=KEY  この GLFW のキーコード（E なら 69 など）のキーを押すと、偽の画面を開いて
//                 カーソルを放す（"SCREEN open"）か、閉じて Minecraft と同じくカーソルをまた捕まえる
//                 （SmokeImage.Screen）。--capture ならゲーム中で、なければ画面を開いて始める
//   --watch-keys=KEY,...  画面を閉じたとき、Minecraft の KeyMapping.setAll と同じく、カーソルを
//                 捕まえる直前に glfwGetKey で読んだこれらの GLFW のキーコードについて
//                 "SETALL <key>=<0|1> ..." を出す
// SMOKE_VERBOSE=1 で「ゲーム」が受け取ったものを出す："KEY key scancode action mods"、
// "BUTTON button action mods"、"FOCUS 0|1"（GLFW のコールバックの引数）。
// 毎フレーム、ゲームの WGL のコンテキストが今のもののままで、GL のエラーがないことを確かめる。
// ゲームのコンテキストに状態を漏らすエージェントは失敗する。デバッグのキーは Minecraft と同じく
// 動く（SmokeImage.DebugKeys）：F3 を押したまま C を押すと、Minecraft の形式の位置を
// glfwSetClipboardString でコピーし（"F3C copied"）、処理したデバッグのキーなしで F3 を離すと
// デバッグ画面を切り替える。最後に "F3C STATE overlay=<on|off> modifier=<up|down> copies=N" を出す。
// "SMOKE OK frames=N" を出して 0 で終わるか、"SMOKE FAIL: <reason>" を出して 1 で終わる。

import java.nio.ByteBuffer;

import org.lwjgl.BufferUtils;
import org.lwjgl.Version;
import org.lwjgl.glfw.Callbacks;
import org.lwjgl.glfw.GLFWErrorCallback;
import org.lwjgl.glfw.GLFWNativeWGL;
import org.lwjgl.opengl.GL;
import org.lwjgl.opengl.WGL;
import org.lwjgl.system.Platform;

import static org.lwjgl.glfw.GLFW.*;
import static org.lwjgl.opengl.GL11.*;

public final class Smoke {
    // クリアの色。8 ビットで正確になるように選んだ：RGB 51 102 153。
    private static final float CLEAR_R = 0.2f, CLEAR_G = 0.4f, CLEAR_B = 0.6f;

    private static int keys, chars, buttons, cursorMoves, scrolls, glfwErrors;
    // キーのイベントが残した F3 の状態（--f3c-events）。
    private static boolean f3Down;
    private static final StringBuilder typed = new StringBuilder();
    // SMOKE_VERBOSE=1 で、「ゲーム」が受け取るキー・マウスのボタン・フォーカスのイベントをすべて出す。
    private static final boolean VERBOSE = System.getenv("SMOKE_VERBOSE") != null;
    private static String lastGlfwError = "none";

    private static final class Fail extends RuntimeException {
        private static final long serialVersionUID = 1L;

        Fail(String reason) {
            super(reason);
        }
    }

    public static void main(String[] args) {
        int code;
        try {
            run(args);
            code = 0;
        } catch (Fail e) {
            System.out.println("SMOKE FAIL: " + e.getMessage());
            code = 1;
        } catch (Throwable t) {
            System.out.println("SMOKE FAIL: " + t);
            t.printStackTrace(System.out);
            code = 1;
        }
        System.out.flush();
        System.exit(code);
    }

    private static void run(String[] args) {
        int frames = 120;
        boolean legacy = false, readback = false, screenshot = false, capture = false, mc = false;
        boolean f3cEvents = false, f3cRefuse = false;
        String world = null;
        double seconds = 0;
        int screenKey = 0;
        int[] watchKeys = null;
        for (String arg : args) {
            if (arg.startsWith("--seconds=")) {
                seconds = Double.parseDouble(arg.substring("--seconds=".length()));
            } else if (arg.startsWith("--screen-key=")) {
                screenKey = SmokeImage.keyCode(arg, GLFW_KEY_SPACE, GLFW_KEY_LAST);
            } else if (arg.startsWith("--watch-keys=")) {
                watchKeys = SmokeImage.keyCodes(arg, GLFW_KEY_SPACE, GLFW_KEY_LAST);
            } else if (arg.startsWith("--world=")) {
                world = arg.substring("--world=".length());
            } else if (arg.equals("--f3c-events")) {
                f3cEvents = true;
            } else if (arg.equals("--f3c-refuse")) {
                f3cRefuse = true;
            } else if (arg.equals("--capture")) {
                capture = true;
            } else if (arg.equals("--mc")) {
                mc = true;
            } else if (arg.equals("--sodium")) {
                SmokeImage.sodium = true;
            } else if (arg.equals("--legacy")) {
                legacy = true;
            } else if (arg.equals("--readback")) {
                readback = true;
            } else if (arg.equals("--screenshot")) {
                screenshot = true;
            } else {
                try {
                    frames = Integer.parseInt(arg);
                } catch (NumberFormatException e) {
                    throw new Fail("unknown argument " + arg);
                }
                if (frames < 1) {
                    throw new Fail("frame count must be positive: " + arg);
                }
            }
        }
        if (watchKeys != null && screenKey == 0) {
            throw new Fail("--watch-keys needs --screen-key");
        }
        if (SmokeImage.sodium && !mc) {
            throw new Fail("--sodium needs --mc");
        }
        long t0 = System.nanoTime();
        System.out.println("LWJGL " + Version.getVersion());
        if (world != null) {
            SmokeImage.writeWorld(world);
        }

        // Minecraft は glfwInit の前にエラーのコールバックを設定する。
        GLFWErrorCallback errorCallback = GLFWErrorCallback.create((error, description) -> {
            glfwErrors++;
            lastGlfwError = String.format("0x%X %s", error, GLFWErrorCallback.getDescription(description));
            System.out.println("GLFW ERROR " + lastGlfwError);
        });
        glfwSetErrorCallback(errorCallback);
        if (!glfwInit()) {
            throw new Fail("glfwInit failed (last GLFW error: " + lastGlfwError + ")");
        }
        System.out.println("GLFW " + glfwGetVersionString());

        glfwDefaultWindowHints();
        if (!legacy) {
            // Minecraft 1.17 以降のウィンドウのヒント。
            glfwWindowHint(GLFW_CLIENT_API, GLFW_OPENGL_API);
            glfwWindowHint(GLFW_CONTEXT_CREATION_API, GLFW_NATIVE_CONTEXT_API);
            glfwWindowHint(GLFW_CONTEXT_VERSION_MAJOR, 3);
            glfwWindowHint(GLFW_CONTEXT_VERSION_MINOR, 2);
            glfwWindowHint(GLFW_OPENGL_PROFILE, GLFW_OPENGL_CORE_PROFILE);
            glfwWindowHint(GLFW_OPENGL_FORWARD_COMPAT, GLFW_TRUE);
        }
        long window = glfwCreateWindow(854, 480, "reminedog smoke", 0, 0);
        if (window == 0) {
            throw new Fail("glfwCreateWindow failed (last GLFW error: " + lastGlfwError + ")");
        }

        // Minecraft は Window のコンストラクタでコンテキストを今のものにして GL の capabilities を作り、
        // その後で入力のコールバックを登録する。
        glfwMakeContextCurrent(window);
        GL.createCapabilities();
        glfwSwapInterval(0); // vsync をオフにして実行を短くする
        System.out.println("GL_VERSION " + glGetString(GL_VERSION));
        System.out.println("GL_RENDERER " + glGetString(GL_RENDERER));
        System.out.println("GL_VENDOR " + glGetString(GL_VENDOR));

        final boolean events = f3cEvents;
        final SmokeImage.DebugKeys debugKeys = new SmokeImage.DebugKeys(f3cRefuse,
            () -> glfwSetClipboardString(window, SmokeImage.DebugKeys.F3C_TEXT));
        final SmokeImage.Screen screen = screenKey == 0 ? null
            : new SmokeImage.Screen(screenKey, !capture, watchKeys != null ? watchKeys : new int[0],
                key -> glfwGetKey(window, key) == GLFW_PRESS,
                () -> setCursor(window, GLFW_CURSOR_NORMAL), () -> setCursor(window, GLFW_CURSOR_DISABLED));
        glfwSetKeyCallback(window, (w, key, scancode, action, mods) -> {
            keys++;
            if (VERBOSE) {
                System.out.printf("KEY %d %d %d %d%n", key, scancode, action, mods);
            }
            // Minecraft 1.16 は F3 を押しているかを glfwGetKey に聞き、1.21.9 以降は F3 のキーのイベントに従う。
            // どちらもクラッシュのキーの本当の状態は glfwGetKey で読む。
            boolean f3Held = events ? f3Down : glfwGetKey(w, GLFW_KEY_F3) == GLFW_PRESS;
            debugKeys.keyEvent(glfwGetKey(w, GLFW_KEY_C) == GLFW_PRESS, f3Held);
            if (key == GLFW_KEY_F3 && action == GLFW_RELEASE) {
                debugKeys.modifierReleased();
            } else if (action != GLFW_RELEASE && f3Held) {
                debugKeys.debugKey(key == GLFW_KEY_C);
            }
            if (key == GLFW_KEY_F3) {
                f3Down = action != GLFW_RELEASE;
            }
            if (screen != null && action == GLFW_PRESS) {
                screen.keyPressed(key);
            }
        });
        glfwSetCharModsCallback(window, (w, codepoint, mods) -> {
            chars++;
            typed.appendCodePoint(codepoint);
        });
        glfwSetMouseButtonCallback(window, (w, button, action, mods) -> {
            buttons++;
            if (VERBOSE) {
                System.out.printf("BUTTON %d %d %d%n", button, action, mods);
            }
        });
        glfwSetCursorPosCallback(window, (w, x, y) -> cursorMoves++);
        glfwSetScrollCallback(window, (w, dx, dy) -> scrolls++);
        final int[] fbSize = new int[2];
        {
            int[] w = new int[1], h = new int[1];
            glfwGetFramebufferSize(window, w, h);
            fbSize[0] = w[0];
            fbSize[1] = h[0];
        }
        glfwSetFramebufferSizeCallback(window, (w, width, height) -> {
            fbSize[0] = width;
            fbSize[1] = height;
        });
        // Minecraft はウィンドウのフォーカスにも従う（エージェントはこれでフォーカスを失ったことを知る）。
        glfwSetWindowFocusCallback(window, (w, focused) -> {
            if (VERBOSE) {
                System.out.println("FOCUS " + (focused ? 1 : 0));
            }
        });
        SmokeImage.MainTarget mainTarget = mc ? new SmokeImage.MainTarget() : null;
        if (capture) {
            glfwSetInputMode(window, GLFW_CURSOR, GLFW_CURSOR_DISABLED);
        }
        if ((capture || screen != null) && glfwRawMouseMotionSupported()) {
            glfwSetInputMode(window, GLFW_RAW_MOUSE_MOTION, GLFW_TRUE);
        }

        // エージェントはスワップの中で GL のコンテキストを切り替える。その後でこちらのものが今のものか
        // 確かめる。
        long wglContext = Platform.get() == Platform.WINDOWS ? GLFWNativeWGL.glfwGetWGLContext(window) : 0;
        int glError = glGetError();
        if (glError != GL_NO_ERROR) {
            throw new Fail(String.format("GL error 0x%X after setup", glError));
        }

        long t1 = System.nanoTime();
        long until = t1 + (long) (seconds * 1e9);
        int frame = 0;
        for (; seconds > 0 ? System.nanoTime() < until : frame < frames; frame++) {
            if (seconds > 0) {
                sleep(15);
            }
            if (mainTarget != null) {
                mainTarget.render(fbSize[0], fbSize[1]);
                mainTarget.present();
            } else {
                glClearColor(CLEAR_R, CLEAR_G, CLEAR_B, 1f);
                glClear(GL_COLOR_BUFFER_BIT);
                SmokeImage.drawPattern(854, 480);
            }
            glfwSwapBuffers(window);
            glfwPollEvents();
            if (wglContext != 0 && WGL.wglGetCurrentContext() != wglContext) {
                throw new Fail("frame " + frame + ": WGL context is no longer current after swap");
            }
            glError = glGetError();
            if (glError != GL_NO_ERROR) {
                throw new Fail(String.format("frame %d: GL error 0x%X", frame, glError));
            }
        }
        long t2 = System.nanoTime();

        if (readback) {
            readback(window);
        }
        if (screenshot) {
            screenshot(window, "smoke-screenshot.png");
        }
        frames = frame;
        System.out.printf("EVENTS key=%d char=%d button=%d cursor=%d scroll=%d glfw_errors=%d%n",
            keys, chars, buttons, cursorMoves, scrolls, glfwErrors);
        System.out.println("TYPED " + typed);
        debugKeys.printState(f3cEvents ? f3Down : glfwGetKey(window, GLFW_KEY_F3) == GLFW_PRESS);
        System.out.printf("TIMING setup_ms=%d loop_ms=%d fps=%.0f%n",
            (t1 - t0) / 1000000, (t2 - t1) / 1000000, frames * 1e9 / Math.max(1, t2 - t1));

        // Minecraft の終了の順序：ウィンドウのコールバックを解放する（LWJGL は glfwSet*Callback の
        // 呼び出しが返したものを解放するので、フックはゲームのポインタを返さなければならない）。
        Callbacks.glfwFreeCallbacks(window);
        glfwDestroyWindow(window);
        glfwTerminate();
        glfwSetErrorCallback(null);
        errorCallback.free();
        System.out.println("SMOKE OK frames=" + frames);
    }

    // 調べるためだけ：最後のスワップの後、いくつかのウィンドウの座標のピクセルをフロントバッファ
    // （"PIXEL"）とバックバッファ（"PIXEL_BACK"）から出す。Wine＋Mesa の llvmpipe では、フロント
    // バッファは黒く読めるが、バックバッファには見せたフレームが残っている（コピーのスワップ）。
    // 本物のドライバでは、スワップの後のバックバッファは未定義。
    private static void readback(long window) {
        int[] winW = new int[1], winH = new int[1], fbW = new int[1], fbH = new int[1];
        glfwGetWindowSize(window, winW, winH);
        glfwGetFramebufferSize(window, fbW, fbH);
        glFinish();
        glPixelStorei(GL_PACK_ALIGNMENT, 1);
        System.out.printf("CLEAR %d %d %d%n",
            Math.round(CLEAR_R * 255), Math.round(CLEAR_G * 255), Math.round(CLEAR_B * 255));
        ByteBuffer rgba = BufferUtils.createByteBuffer(4);
        int[][] points = {{40, 40}, {120, 80}, {winW[0] / 2, winH[0] / 2}};
        int[] buffers = {GL_FRONT, GL_BACK};
        for (int buffer : buffers) {
            glReadBuffer(buffer);
            for (int[] p : points) {
                // ウィンドウの座標は左上が原点。GL のものは左下。
                int x = p[0] * fbW[0] / Math.max(1, winW[0]);
                int y = fbH[0] - 1 - p[1] * fbH[0] / Math.max(1, winH[0]);
                glReadPixels(x, y, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, rgba);
                System.out.printf("%s %d %d %d %d %d%n", buffer == GL_FRONT ? "PIXEL" : "PIXEL_BACK",
                    p[0], p[1], rgba.get(0) & 0xFF, rgba.get(1) & 0xFF, rgba.get(2) & 0xFF);
            }
        }
        glReadBuffer(GL_BACK);
        int glError = glGetError();
        if (glError != GL_NO_ERROR) {
            throw new Fail(String.format("GL error 0x%X during readback", glError));
        }
    }

    private static void screenshot(long window, String path) {
        int[] w = new int[1], h = new int[1];
        glfwGetFramebufferSize(window, w, h);
        SmokeImage.saveBackBuffer(w[0], h[0], path);
    }

    // Minecraft の InputConstants.grabOrReleaseMouse：カーソルをウィンドウの中央へ動かし、それから
    // カーソルのモード。
    private static void setCursor(long window, int mode) {
        int[] w = new int[1], h = new int[1];
        glfwGetWindowSize(window, w, h);
        glfwSetCursorPos(window, w[0] / 2.0, h[0] / 2.0);
        glfwSetInputMode(window, GLFW_CURSOR, mode);
    }

    private static void sleep(long millis) {
        try {
            Thread.sleep(millis);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
    }
}
