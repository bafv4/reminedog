// SDL3 counterpart of Smoke.java: Minecraft 26.x opens its window with SDL3 (LWJGL 3.4)
// instead of GLFW. Follows what 26.3 does: it creates its OpenGL context on a hidden
// utility window, then creates the visible game window and makes that context current on
// it. Java 8 source: javac --release 8.
//
// Usage: java -cp <lwjgl jars incl. lwjgl-sdl>;<classes> SmokeSdl [FRAMES] [--screenshot]
//                                                                [--seconds=N] [--capture]
//   --screenshot  save the back buffer after the last swap as smoke-screenshot.png
//   --seconds=N   run for N seconds at about 60 fps instead of a frame count (input tests)
//   --capture     relative mouse mode, as Minecraft uses in game
// Prints "SMOKE OK frames=N" and exits 0, or "SMOKE FAIL: <reason>" and exits 1.

import org.lwjgl.Version;
import org.lwjgl.opengl.GL;
import org.lwjgl.sdl.SDL_Event;

import static org.lwjgl.opengl.GL11.*;
import static org.lwjgl.sdl.SDLError.SDL_GetError;
import static org.lwjgl.sdl.SDLEvents.*;
import static org.lwjgl.sdl.SDLMouse.SDL_SetWindowRelativeMouseMode;
import static org.lwjgl.sdl.SDLInit.*;
import static org.lwjgl.sdl.SDLVideo.*;

public final class SmokeSdl {
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
        boolean screenshot = false, capture = false;
        double seconds = 0;
        for (String arg : args) {
            if (arg.startsWith("--seconds=")) {
                seconds = Double.parseDouble(arg.substring("--seconds=".length()));
                continue;
            }
            if (arg.equals("--capture")) {
                capture = true;
                continue;
            }
            if (arg.equals("--screenshot")) {
                screenshot = true;
                continue;
            }
            if (arg.startsWith("--")) {
                continue; // Smoke.java's other flags; not supported here
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
        System.out.println("LWJGL " + Version.getVersion());
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
        System.out.println("GL_VERSION " + glGetString(GL_VERSION));
        System.out.println("GL_RENDERER " + glGetString(GL_RENDERER));

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
                glClearColor(0.2f, 0.4f, 0.6f, 1f);
                glClear(GL_COLOR_BUFFER_BIT);
                SmokeImage.drawPattern(854, 480);
                if (!SDL_GL_SwapWindow(window)) {
                    throw new Fail("frame " + frame + ": SDL_GL_SwapWindow failed: " + SDL_GetError());
                }
                while (SDL_PollEvent(event)) {
                    switch (event.type()) {
                        case SDL_EVENT_KEY_DOWN:
                        case SDL_EVENT_KEY_UP:
                            keys++;
                            break;
                        case SDL_EVENT_TEXT_INPUT:
                            texts++;
                            typed.append(event.text().textString());
                            break;
                        case SDL_EVENT_MOUSE_BUTTON_DOWN:
                        case SDL_EVENT_MOUSE_BUTTON_UP:
                            buttons++;
                            break;
                        case SDL_EVENT_MOUSE_MOTION:
                            motions++;
                            break;
                        case SDL_EVENT_MOUSE_WHEEL:
                            wheels++;
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
        if (screenshot) {
            SmokeImage.saveBackBuffer(854, 480, "smoke-screenshot.png");
        }
        SDL_GL_DestroyContext(context);
        SDL_DestroyWindow(window);
        SDL_DestroyWindow(utility);
        SDL_Quit();
        System.out.println("SMOKE OK frames=" + frames);
    }
}
