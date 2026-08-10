package dev.bolide.ide;

public final class BolideNative {
    static {
        System.loadLibrary("bolide_android");
    }

    private BolideNative() {}

    public interface IoBridge {
        void onNativeOutput(String text);
        String onNativeInput(String prompt);
        boolean onNativeGuiRequest(String title);
        void onNativeGuiClosed();
    }

    public static native String runFile(String path, String bolideHome, IoBridge bridge);
    public static native String evalRepl(
            String code, String baseDir, String bolideHome, IoBridge bridge);
    public static native void resetRepl();
    public static native boolean closeGui();
    public static native void registerGuiActivity(BolideGuiActivity activity);
    public static native void setGuiInsets(int left, int top, int right, int bottom);
    public static native void guiActivityDestroyed();
}
