package com.arn.scrobble;

import java.util.ArrayList;
import java.util.Arrays;

// all params should be non null
// this class is just useed for testing

public class PanoNativeComponents {
    private static native void setLogFilePath(String path);

    private static native void startListeningMedia();

    static native void setEnvironmentVariable(String key, String value);

    static native void refreshSessions();

    static native void stopListeningMedia();

    static native void skip(String appId);

    static native void mute(String appId);

    static native void unmute(String appId);

    static native void notify(String title, String body);

    static native void setTrayLinux(String tooltip, byte[] pngBytes, boolean invert, String[] menuItemIds, String[] menuItemTexts);

    static native String getMachineId();

    static native void applyWindowEffects(long handle, boolean isDark, boolean isBlur);

    static native boolean sendIpcCommand(String command, String arg);

    static native boolean isFileLockedWindows(String path);

    static native void fileChooser(int requestId, boolean save, String title, String fileName, String[] filters);

    static native void onFilePicked(int requestId, String uri);

    static native boolean updateDiscordActivity(String clientId, String name, String state, String details, String largeText, long startTime, long endTime, String artUrl, String detailsUrl, boolean isPlaying, int statusLine, String buttonText, String buttonUrl);

    static native boolean clearDiscordActivity(boolean shutdown);

    static native boolean autoStartLinux(boolean add);

    static native void openUrl(String url);

    static {
        System.loadLibrary("pano_native_components");
    }

    private static ArrayList<String> appIds = new ArrayList<>(Arrays.asList(
        "Spotify.exe",
        "MusicBee.exe",
        "foobar2000.exe",
        "AppleInc.AppleMusicWin_nzyj5cx40ttqa!App",
        "com.squirrel.TIDAL.TIDAL",
        "com.deezer.deezer-desktop",
        "org.mpris.MediaPlayer2.Lollypop",
        "org.mpris.MediaPlayer2.elisa",
        "org.mpris.MediaPlayer2.plasma-browser-integration",
        "com.apple.Music",
        "org.mpris.MediaPlayer2.cider",
        "org.mpris.MediaPlayer2.cider.instancen"
    ));

    public static void main(String[] args) {
        applyDarkModeWindows(0);
        setEnvironmentVariable("GDK_BACKEND", "x11");

        sendIpcCommand("testCommand", "testArg");

        new Thread(new Runnable() {
            @Override
            public void run() {
                while (true) {
                    try {
                        Thread.sleep(1000);
                    } catch (InterruptedException e) {
                        e.printStackTrace();
                    }
                    
                    System.out.println("startListeningMedia");
                    startListeningMedia();
                    System.out.println("startListeningMedia finished");
                }
            }
        }).start();

        new Thread(new Runnable() {
            @Override
            public void run() {
                try {
                    Thread.sleep(5000);
                } catch (InterruptedException e) {
                    e.printStackTrace();
                }

                PanoNativeComponents.notify("Test Notification", "This is a test notification");

                // test tray icon
                String menuItemIds[] = new String[] { "1", "2", "3", "Separator", "4" };
                String menuItemTexts[] = new String[] { "📝 item_1", "item_2", "item_3", "", "item_4" };

                int size = 8;
                byte[] argb = new byte[size * size * 4];
                for (int i = 0; i < argb.length; i += 4) {
                    argb[i] = (byte) 0xff;
                    argb[i + 1] = (byte) 0xbe;
                    argb[i + 2] = (byte) 0xbe;
                    argb[i + 3] = (byte) 0xbe;
                }
                setTrayLinux("", argb, false, menuItemIds, menuItemTexts);

                // refreshSessions();

                try {
                    Thread.sleep(5000);
                } catch (InterruptedException e) {
                    e.printStackTrace();
                }

                System.out.println("shutting down");
                // setAllowedAppIds(new String[] {});
            }
        }).start();

    }

    public static void onActiveSessionsChanged(String[] appIds, String[] appNames) {
        System.out.println("onActiveSessionsChanged: ");
        for (int i = 0; i < appIds.length; i++) {
            System.out.println("App ID: " + appIds[i] + ", App Name: " + appNames[i]);
        }
    }

    public static void onMetadataChanged(String appId, String title, String artist, String album, String albumArtist, int trackNumber, long duration, String artUrl, String trackUrl) {
        System.out.println("onMetadataChanged: " + appId + ", " + title + ", " + artist + ", " + album + ", " + albumArtist + ", " + trackNumber + ", " + duration + ", " + artUrl + ", " + trackUrl);
    }

    public static void onPlaybackStateChanged(String appId, String state, long position, boolean canSkip) {
        System.out.println("onPlaybackStateChanged: " + appId + ", " + state + ", " + position + ", " + canSkip);
    }

    public static void onTrayMenuItemClicked(String id) {
        System.out.println("onTrayMenuItemClicked: " + id);
    }

    public static void onReceiveIpcCommand(String command, String arg) {
        System.out.println("onReceiveIpcCommand: " + command + " " + arg);
    }

    public static void onDarkModeChange(boolean isDarkMode) {
        System.out.println("onDarkModeChange: " + isDarkMode);
    }

    public static boolean isAppIdAllowed(String appId) {
        return appIds.contains(appId);
    }

}
