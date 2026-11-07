# Add project specific ProGuard rules here.
# You can control the set of applied configuration files using the
# proguardFiles setting in build.gradle.
#
# For more details, see
#   http://developer.android.com/guide/developing/tools/proguard.html

# If your project uses WebView with JS, uncomment the following
# and specify the fully qualified class name to the JavaScript interface
# class:
#-keepclassmembers class fqcn.of.javascript.interface.for.webview {
#   public *;
#}

# Uncomment this to preserve the line number information for
# debugging stack traces.
#-keepattributes SourceFile,LineNumberTable

# If you keep the line number information, uncomment this to
# hide the original source file name.
#-renamesourcefileattribute SourceFile

# The Rust side finds the app's plugin class by its name and builds it with the activity, and Tauri's plugin manager finds its
# commands by their annotations at run time. The rules that ship with the Tauri library keep a public class that carries
# @TauriPlugin the same way; this one says it for the app's own class by name, so that a change to those rules cannot drop it
# silently, and keeps the annotations the manager reads. The service is named in the manifest, which keeps it.
-keepattributes RuntimeVisibleAnnotations,AnnotationDefault,Signature
-keep class ch.holzcloud.holzwave.HolzwavePlugin {
  public <init>(android.app.Activity);
  @app.tauri.annotation.Command public <methods>;
}
-keep @app.tauri.annotation.InvokeArg class ch.holzcloud.holzwave.** {
  *;
}
