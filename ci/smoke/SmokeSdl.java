// Smoke.java の SDL3 版：Minecraft 26.x は GLFW の代わりに SDL3（LWJGL 3.4）でウィンドウを開く。
// 26.3 のすることに従う：表示しない補助のウィンドウで OpenGL のコンテキストを作り、それから
// 見えるゲームのウィンドウを作って、そこでそのコンテキストを今のものにする。Java 8 のソース：
// javac --release 8。
//
// 使い方：java -cp <lwjgl jars incl. lwjgl-sdl>;<classes> SmokeSdl [FRAMES] [--screenshot]
//                                                                [--seconds=N] [--capture] [--mc [--sodium]]
//                                                                [--world=NAME] [--f3c-refuse]
//                                                                [--screen-key=SCANCODE
//                                                                 [--watch-keys=SCANCODE,...]]
//   --screenshot  最後のスワップの後、バックバッファを smoke-screenshot.png として保存する
//   --seconds=N   フレームの数の代わりに、約 60 fps で N 秒動かす（入力のテスト用）
//   --capture     ゲーム中の Minecraft と同じく、相対マウスモードにする
//   --mc          Minecraft と同じく描く：ウィンドウのイベントが報告するピクセルの大きさで自前の
//                 フレームバッファに描き、ウィンドウに写す
//   --sodium      --mc と一緒に、Sodium を入れた Minecraft と同じく viewport を設定する（直前と同じ
//                 glViewport を省く。SmokeImage.viewport）
//   --world=NAME  始めに、シングルプレイのワールドを開いたときの Minecraft と同じく、
//                 saves/NAME/session.lock と、統合サーバーの開始の行を持つ logs/latest.log を今の
//                 フォルダに書く（エージェントがワールドを知るように）
//   --f3c-refuse  デバッグ情報を制限したワールドと同じく、F3+C で何もコピーしない
//   --screen-key=SCANCODE  ゲームのウィンドウでこの SDL のスキャンコード（E なら 8 など）のキーを
//                 押すと、偽の画面を開いて相対マウスモードを抜ける（"SCREEN open"）か、閉じて
//                 Minecraft と同じくまた入る（SmokeImage.Screen）。--capture ならゲーム中で、なければ
//                 画面を開いて始める
//   --watch-keys=SCANCODE,...  画面を閉じたとき、Minecraft の KeyMapping.setAll と同じく、相対
//                 マウスモードに入る直前に SDL_GetKeyboardState で読んだこれらのスキャンコードに
//                 ついて "SETALL <scancode>=<0|1> ..." を出す
// デバッグのキーは 26.3 と同じく動く（SmokeImage.DebugKeys）：ゲームのウィンドウ（windowID で
// 見分ける）のキーのイベントで F3 を押しているかを決め、そのとき C を押すと Minecraft の形式の
// 位置を SDL_SetClipboardText でコピーし（"F3C copied"）、処理したデバッグのキーなしで F3 を
// 離すとデバッグ画面を切り替える。最後に "F3C STATE overlay=<on|off> modifier=<up|down> copies=N"
// を出す。SMOKE_VERBOSE=1 で、どのウィンドウのものでも「ゲーム」が受け取るイベントを出す：
// "KEY scancode keycode action mod windowID"（action は 1 が押下、0 が解放、-1 がリピート。
// keycode は符号なし）、"BUTTON button down windowID"（down は 1 か 0。26.3 と同じくイベントの
// 種類から）、"FOCUS 0|1"。
// "SMOKE OK frames=N" を出して 0 で終わるか、"SMOKE FAIL: <reason>" を出して 1 で終わる。

import java.nio.ByteBuffer;
import java.nio.IntBuffer;

import org.lwjgl.BufferUtils;
import org.lwjgl.Version;
import org.lwjgl.opengl.GL;
import org.lwjgl.opengl.WGL;
import org.lwjgl.system.Platform;
import org.lwjgl.sdl.SDL_Event;
import org.lwjgl.sdl.SDL_KeyboardEvent;
import org.lwjgl.sdl.SDL_MouseButtonEvent;

import static org.lwjgl.opengl.GL11.*;
import static org.lwjgl.sdl.SDLClipboard.SDL_SetClipboardText;
import static org.lwjgl.sdl.SDLError.SDL_GetError;
import static org.lwjgl.sdl.SDLEvents.*;
import static org.lwjgl.sdl.SDLKeyboard.SDL_GetKeyboardState;
import static org.lwjgl.sdl.SDLMouse.SDL_SetWindowRelativeMouseMode;
import static org.lwjgl.sdl.SDLMouse.SDL_WarpMouseInWindow;
import static org.lwjgl.sdl.SDLInit.*;
import static org.lwjgl.sdl.SDLScancode.SDL_SCANCODE_C;
import static org.lwjgl.sdl.SDLScancode.SDL_SCANCODE_COUNT;
import static org.lwjgl.sdl.SDLScancode.SDL_SCANCODE_F3;
import static org.lwjgl.sdl.SDLVideo.*;

public final class SmokeSdl {
    private static final boolean VERBOSE = System.getenv("SMOKE_VERBOSE") != null;

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
        boolean screenshot = false, capture = false, mc = false, f3cRefuse = false;
        String world = null;
        double seconds = 0;
        int screenKey = 0;
        int[] watchKeys = null;
        for (String arg : args) {
            if (arg.startsWith("--seconds=")) {
                seconds = Double.parseDouble(arg.substring("--seconds=".length()));
                continue;
            }
            if (arg.startsWith("--screen-key=")) {
                screenKey = SmokeImage.keyCode(arg, 1, SDL_SCANCODE_COUNT - 1);
                continue;
            }
            if (arg.startsWith("--watch-keys=")) {
                watchKeys = SmokeImage.keyCodes(arg, 1, SDL_SCANCODE_COUNT - 1);
                continue;
            }
            if (arg.startsWith("--world=")) {
                world = arg.substring("--world=".length());
                continue;
            }
            if (arg.equals("--f3c-refuse")) {
                f3cRefuse = true;
                continue;
            }
            if (arg.equals("--capture")) {
                capture = true;
                continue;
            }
            if (arg.equals("--mc")) {
                mc = true;
                continue;
            }
            if (arg.equals("--sodium")) {
                SmokeImage.sodium = true;
                continue;
            }
            if (arg.equals("--screenshot")) {
                screenshot = true;
                continue;
            }
            try {
                frames = Integer.parseInt(arg);
            } catch (NumberFormatException e) {
                throw new Fail("unknown argument " + arg);
            }
            if (frames < 1) {
                throw new Fail("frame count must be positive: " + arg);
            }
        }
        if (watchKeys != null && screenKey == 0) {
            throw new Fail("--watch-keys needs --screen-key");
        }
        if (SmokeImage.sodium && !mc) {
            throw new Fail("--sodium needs --mc");
        }
        System.out.println("LWJGL " + Version.getVersion());
        if (world != null) {
            SmokeImage.writeWorld(world);
        }
        if (!SDL_Init(SDL_INIT_VIDEO)) {
            throw new Fail("SDL_Init failed: " + SDL_GetError());
        }
        SDL_GL_SetAttribute(SDL_GL_CONTEXT_MAJOR_VERSION, 3);
        SDL_GL_SetAttribute(SDL_GL_CONTEXT_MINOR_VERSION, 2);
        SDL_GL_SetAttribute(SDL_GL_CONTEXT_PROFILE_MASK, SDL_GL_CONTEXT_PROFILE_CORE);
        long utility = SDL_CreateWindow("reminedog smoke (SDL3) hidden utility window", 320, 480,
            SDL_WINDOW_OPENGL | SDL_WINDOW_HIDDEN);
        if (utility == 0) {
            throw new Fail("SDL_CreateWindow (utility) failed: " + SDL_GetError());
        }
        long context = SDL_GL_CreateContext(utility);
        if (context == 0) {
            throw new Fail("SDL_GL_CreateContext failed: " + SDL_GetError());
        }
        long window = SDL_CreateWindow("reminedog smoke (SDL3)", 854, 480,
            SDL_WINDOW_OPENGL | SDL_WINDOW_RESIZABLE);
        if (window == 0) {
            throw new Fail("SDL_CreateWindow failed: " + SDL_GetError());
        }
        if (!SDL_GL_MakeCurrent(window, context)) {
            throw new Fail("SDL_GL_MakeCurrent failed: " + SDL_GetError());
        }
        SDL_GL_SetSwapInterval(0);
        if (capture) {
            SDL_SetWindowRelativeMouseMode(window, true);
        }
        GL.createCapabilities();
        // ゲームの WGL のコンテキストは、エージェントのスワップをまたいで今のもののままでなければ
        // ならない（エージェントは自前のコンテキストで描いて戻す）。
        // LWJGL 3.4 は、呼び出しの GetLastError のためのバッファを取る。
        IntBuffer lastError = BufferUtils.createIntBuffer(1);
        long wglContext = Platform.get() == Platform.WINDOWS ? WGL.wglGetCurrentContext(lastError) : 0;
        System.out.println("GL_VERSION " + glGetString(GL_VERSION));
        System.out.println("GL_RENDERER " + glGetString(GL_RENDERER));

        int[] fbW = new int[1], fbH = new int[1];
        {
            java.nio.IntBuffer w = org.lwjgl.BufferUtils.createIntBuffer(1);
            java.nio.IntBuffer h = org.lwjgl.BufferUtils.createIntBuffer(1);
            SDL_GetWindowSizeInPixels(window, w, h);
            fbW[0] = w.get(0);
            fbH[0] = h.get(0);
        }
        SmokeImage.MainTarget mainTarget = mc ? new SmokeImage.MainTarget() : null;
        int windowId = SDL_GetWindowID(window);
        SmokeImage.DebugKeys debugKeys = new SmokeImage.DebugKeys(f3cRefuse, () -> {
            if (!SDL_SetClipboardText(SmokeImage.DebugKeys.F3C_TEXT)) {
                System.out.println("SDL_SetClipboardText failed: " + SDL_GetError());
            }
        });
        SmokeImage.Screen screen = screenKey == 0 ? null
            : new SmokeImage.Screen(screenKey, !capture, watchKeys != null ? watchKeys : new int[0],
                SmokeSdl::keyDown, () -> grabMouse(window, false), () -> grabMouse(window, true));
        boolean f3Down = false;
        int keys = 0, texts = 0, buttons = 0, motions = 0, wheels = 0;
        StringBuilder typed = new StringBuilder();
        long until = System.nanoTime() + (long) (seconds * 1e9);
        int frame = 0;
        SDL_Event event = SDL_Event.calloc();
        try {
            for (; seconds > 0 ? System.nanoTime() < until : frame < frames; frame++) {
                if (seconds > 0) {
                    try {
                        Thread.sleep(15);
                    } catch (InterruptedException e) {
                        Thread.currentThread().interrupt();
                    }
                }
                if (mainTarget != null) {
                    mainTarget.render(fbW[0], fbH[0]);
                    mainTarget.present();
                } else {
                    glClearColor(0.2f, 0.4f, 0.6f, 1f);
                    glClear(GL_COLOR_BUFFER_BIT);
                    SmokeImage.drawPattern(854, 480);
                }
                if (!SDL_GL_SwapWindow(window)) {
                    throw new Fail("frame " + frame + ": SDL_GL_SwapWindow failed: " + SDL_GetError());
                }
                if (wglContext != 0 && WGL.wglGetCurrentContext(lastError) != wglContext) {
                    throw new Fail("frame " + frame + ": WGL context is no longer current after swap");
                }
                while (SDL_PollEvent(event)) {
                    switch (event.type()) {
                        case SDL_EVENT_KEY_DOWN:
                        case SDL_EVENT_KEY_UP: {
                            keys++;
                            SDL_KeyboardEvent key = event.key();
                            boolean down = event.type() == SDL_EVENT_KEY_DOWN;
                            if (VERBOSE) {
                                System.out.printf("KEY %d %d %d %d %d%n", key.scancode(),
                                    Integer.toUnsignedLong(key.key()), down ? (key.repeat() ? -1 : 1) : 0,
                                    key.mod() & 0xFFFF, key.windowID());
                            }
                            // 26.3 は自分のウィンドウ（windowID で見つける）のキーのイベントだけを処理し、そこから F3 に
                            // 従い、クラッシュのキーの本当の状態を読む。
                            if (key.windowID() != windowId) {
                                break;
                            }
                            debugKeys.keyEvent(keyDown(SDL_SCANCODE_C), f3Down);
                            if (!down && key.scancode() == SDL_SCANCODE_F3) {
                                debugKeys.modifierReleased();
                            } else if (down && f3Down) {
                                debugKeys.debugKey(key.scancode() == SDL_SCANCODE_C);
                            }
                            if (key.scancode() == SDL_SCANCODE_F3) {
                                f3Down = down;
                            }
                            if (screen != null && down && !key.repeat()) {
                                screen.keyPressed(key.scancode());
                            }
                            break;
                        }
                        case SDL_EVENT_TEXT_INPUT:
                            texts++;
                            typed.append(event.text().textString());
                            break;
                        case SDL_EVENT_MOUSE_BUTTON_DOWN:
                        case SDL_EVENT_MOUSE_BUTTON_UP:
                            buttons++;
                            if (VERBOSE) {
                                // 押しているかはイベントの種類から（26.3 が読むもの）。
                                SDL_MouseButtonEvent button = event.button();
                                System.out.printf("BUTTON %d %d %d%n", button.button() & 0xFF,
                                    event.type() == SDL_EVENT_MOUSE_BUTTON_DOWN ? 1 : 0, button.windowID());
                            }
                            break;
                        case SDL_EVENT_WINDOW_FOCUS_GAINED:
                        case SDL_EVENT_WINDOW_FOCUS_LOST:
                            if (VERBOSE) {
                                boolean gained = event.type() == SDL_EVENT_WINDOW_FOCUS_GAINED;
                                System.out.println("FOCUS " + (gained ? 1 : 0));
                            }
                            break;
                        case SDL_EVENT_MOUSE_MOTION:
                            motions++;
                            break;
                        case SDL_EVENT_MOUSE_WHEEL:
                            wheels++;
                            break;
                        case SDL_EVENT_WINDOW_PIXEL_SIZE_CHANGED:
                            fbW[0] = event.window().data1();
                            fbH[0] = event.window().data2();
                            break;
                        default:
                            break;
                    }
                }
                int error = glGetError();
                if (error != GL_NO_ERROR) {
                    throw new Fail(String.format("frame %d: GL error 0x%X", frame, error));
                }
            }
        } finally {
            event.free();
        }
        frames = frame;
        System.out.printf("EVENTS key=%d char=%d button=%d cursor=%d scroll=%d%n",
            keys, texts, buttons, motions, wheels);
        System.out.println("TYPED " + typed);
        debugKeys.printState(f3Down);
        if (screenshot) {
            SmokeImage.saveBackBuffer(854, 480, "smoke-screenshot.png");
        }
        SDL_GL_DestroyContext(context);
        SDL_DestroyWindow(window);
        SDL_DestroyWindow(utility);
        SDL_Quit();
        System.out.println("SMOKE OK frames=" + frames);
    }

    // SDL 自身のキーボードの状態。注入したイベントでは変わらない（26.3 の isKeyDown）。
    private static boolean keyDown(int scancode) {
        ByteBuffer state = SDL_GetKeyboardState();
        return state != null && scancode < state.limit() && state.get(scancode) != 0;
    }

    // 26.3 の InputConstants.grabMouse / releaseMouse：ポインターをウィンドウの中央へ動かし、それから
    // 相対マウスモードをオンかオフにする。
    private static void grabMouse(long window, boolean grab) {
        IntBuffer w = BufferUtils.createIntBuffer(1), h = BufferUtils.createIntBuffer(1);
        SDL_GetWindowSize(window, w, h);
        SDL_WarpMouseInWindow(window, w.get(0) / 2f, h.get(0) / 2f);
        SDL_SetWindowRelativeMouseMode(window, grab);
    }
}
