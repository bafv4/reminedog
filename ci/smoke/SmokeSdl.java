// SDL3 counterpart of Smoke.java: Minecraft 26.x opens its window with SDL3 (LWJGL 3.4)
// instead of GLFW. Follows what 26.3 does: it creates its OpenGL context on a hidden
// utility window, then creates the visible game window and makes that context current on
// it. Java 8 source: javac --release 8.
//
// Usage: java -cp <lwjgl jars incl. lwjgl-sdl>;<classes> SmokeSdl [FRAMES] [--screenshot]
//                                                                [--seconds=N] [--capture] [--mc [--sodium]]
//                                                                [--world=NAME] [--f3c-refuse]
//                                                                [--screen-key=SCANCODE
//                                                                 [--watch-keys=SCANCODE,...]]
//   --screenshot  save the back buffer after the last swap as smoke-screenshot.png
//   --seconds=N   run for N seconds at about 60 fps instead of a frame count (input tests)
//   --capture     relative mouse mode, as Minecraft uses in game
//   --mc          render like Minecraft: into its own framebuffer at the pixel size the
//                 window events report, then copied into the window
//   --sodium      with --mc, set the viewport like Minecraft with Sodium, which skips a
//                 glViewport equal to the last one (SmokeImage.viewport)
//   --world=NAME  at start, write saves/NAME/session.lock and a logs/latest.log with the
//                 integrated server's start line into the current directory, as Minecraft
//                 does when it opens a singleplayer world (so the agent knows the world)
//   --f3c-refuse  F3+C copies nothing, like a world with reduced debug info
//   --screen-key=SCANCODE  pressing the key with this SDL scancode (e.g. 8 for E) in the game
//                 window opens a fake screen, which leaves relative mouse mode ("SCREEN open"),
//                 or closes it, which enters it again like Minecraft (SmokeImage.Screen). The
//                 run starts in game with --capture, else on a screen
//   --watch-keys=SCANCODE,...  when the screen closes, print "SETALL <scancode>=<0|1> ..." for
//                 these scancodes as SDL_GetKeyboardState reads them just before relative mouse
//                 mode is entered, as Minecraft's KeyMapping.setAll does
// The debug keys act like 26.3's (SmokeImage.DebugKeys): F3 is held when the key events of
// the game window (by windowID) say so, pressing C then copies a Minecraft-style location
// with SDL_SetClipboardText ("F3C copied"), and releasing F3 without a handled debug key
// toggles the debug overlay; at the end it prints
// "F3C STATE overlay=<on|off> modifier=<up|down> copies=N". SMOKE_VERBOSE=1 prints the
// events the "game" receives, of any window: "KEY scancode keycode action mod windowID"
// (action 1 press, 0 release, -1 repeat; keycode unsigned), "BUTTON button down windowID"
// (down 1 or 0, from the event type as 26.3 reads it) and "FOCUS 0|1".
// Prints "SMOKE OK frames=N" and exits 0, or "SMOKE FAIL: <reason>" and exits 1.

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
        // The game's WGL context must stay current across the agent's swaps (it draws with its
        // own context and switches back).
        long wglContext = Platform.get() == Platform.WINDOWS ? WGL.wglGetCurrentContext() : 0;
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
                if (wglContext != 0 && WGL.wglGetCurrentContext() != wglContext) {
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
                            // 26.3 handles the key events of its own window only (found by
                            // windowID), follows F3 from them and reads the crash key's real state.
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
                                // Down from the event type, which is what 26.3 reads.
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

    // SDL's own keyboard state, which injected events do not change (26.3's isKeyDown).
    private static boolean keyDown(int scancode) {
        ByteBuffer state = SDL_GetKeyboardState();
        return state != null && scancode < state.limit() && state.get(scancode) != 0;
    }

    // 26.3's InputConstants.grabMouse / releaseMouse: the pointer to the window's centre, then
    // relative mouse mode on or off.
    private static void grabMouse(long window, boolean grab) {
        IntBuffer w = BufferUtils.createIntBuffer(1), h = BufferUtils.createIntBuffer(1);
        SDL_GetWindowSize(window, w, h);
        SDL_WarpMouseInWindow(window, w.get(0) / 2f, h.get(0) / 2f);
        SDL_SetWindowRelativeMouseMode(window, grab);
    }
}
