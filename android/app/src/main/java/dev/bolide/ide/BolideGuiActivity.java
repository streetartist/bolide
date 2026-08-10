package dev.bolide.ide;

import android.app.NativeActivity;
import android.content.Intent;
import android.os.Build;
import android.os.Bundle;
import android.view.DisplayCutout;
import android.view.View;
import android.view.WindowInsets;
import android.window.OnBackInvokedCallback;

import java.lang.ref.WeakReference;

/**
 * Dedicated host for winit/eframe. It intentionally stays alive behind the
 * IDE so the process-wide winit event loop can be reused by later gui.run calls.
 */
public final class BolideGuiActivity extends NativeActivity {
    public static final String EXTRA_TITLE = "dev.bolide.ide.GUI_TITLE";
    private static WeakReference<BolideGuiActivity> currentActivity = new WeakReference<>(null);

    private OnBackInvokedCallback backCallback;
    private boolean returningToIde;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        currentActivity = new WeakReference<>(this);
        BolideNative.registerGuiActivity(this);
        installSafeAreaListener();
        if (Build.VERSION.SDK_INT >= 33) {
            backCallback = this::closeSessionAndReturn;
            getOnBackInvokedDispatcher().registerOnBackInvokedCallback(
                    android.window.OnBackInvokedDispatcher.PRIORITY_DEFAULT, backCallback);
        }
        applyTitle(getIntent());
    }

    private void installSafeAreaListener() {
        View decor = getWindow().getDecorView();
        decor.setOnApplyWindowInsetsListener((view, windowInsets) -> {
            int left;
            int top;
            int right;
            int bottom;
            if (Build.VERSION.SDK_INT >= 30) {
                android.graphics.Insets safe = windowInsets.getInsets(
                        WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
                left = safe.left;
                top = safe.top;
                right = safe.right;
                bottom = safe.bottom;
            } else {
                left = windowInsets.getSystemWindowInsetLeft();
                top = windowInsets.getSystemWindowInsetTop();
                right = windowInsets.getSystemWindowInsetRight();
                bottom = windowInsets.getSystemWindowInsetBottom();
                if (Build.VERSION.SDK_INT >= 28) {
                    DisplayCutout cutout = windowInsets.getDisplayCutout();
                    if (cutout != null) {
                        left = Math.max(left, cutout.getSafeInsetLeft());
                        top = Math.max(top, cutout.getSafeInsetTop());
                        right = Math.max(right, cutout.getSafeInsetRight());
                        bottom = Math.max(bottom, cutout.getSafeInsetBottom());
                    }
                }
            }
            BolideNative.setGuiInsets(left, top, right, bottom);
            return windowInsets;
        });
        decor.requestApplyInsets();
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        applyTitle(intent);
    }

    private void applyTitle(Intent intent) {
        String title = intent == null ? null : intent.getStringExtra(EXTRA_TITLE);
        setTitle(title == null || title.isEmpty() ? "Bolide GUI" : title);
    }

    @SuppressWarnings("deprecation")
    @Override
    public void onBackPressed() {
        closeSessionAndReturn();
    }

    private void closeSessionAndReturn() {
        BolideNative.closeGui();
        returnToIdeFromNative();
    }

    /** Called through a JNI-held reference when egui's own toolbar is tapped. */
    public void returnToIdeFromNative() {
        runOnUiThread(() -> {
            if (returningToIde) return;
            returningToIde = true;
            Intent intent = new Intent(this, MainActivity.class);
            intent.addFlags(Intent.FLAG_ACTIVITY_REORDER_TO_FRONT
                    | Intent.FLAG_ACTIVITY_SINGLE_TOP);
            startActivity(intent);
        });
    }

    public static boolean returnCurrentToIde() {
        BolideGuiActivity activity = currentActivity.get();
        if (activity == null || activity.isDestroyed()) return false;
        activity.returnToIdeFromNative();
        return true;
    }

    @Override
    protected void onResume() {
        super.onResume();
        returningToIde = false;
    }

    @Override
    protected void onDestroy() {
        if (Build.VERSION.SDK_INT >= 33 && backCallback != null) {
            getOnBackInvokedDispatcher().unregisterOnBackInvokedCallback(backCallback);
        }
        if (currentActivity.get() == this) {
            currentActivity.clear();
        }
        BolideNative.guiActivityDestroyed();
        super.onDestroy();
    }
}
