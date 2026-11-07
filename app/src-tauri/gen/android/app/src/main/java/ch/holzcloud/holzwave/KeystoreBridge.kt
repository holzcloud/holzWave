package ch.holzcloud.holzwave

import android.app.Activity
import android.app.KeyguardManager
import android.content.Context
import android.content.pm.PackageManager
import android.hardware.biometrics.BiometricManager
import android.hardware.biometrics.BiometricPrompt
import android.os.CancellationSignal
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyInfo
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import java.nio.ByteBuffer
import java.security.GeneralSecurityException
import java.security.KeyStore
import java.security.ProviderException
import java.security.UnrecoverableKeyException
import javax.crypto.AEADBadTagException
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.SecretKeyFactory
import javax.crypto.spec.GCMParameterSpec

/**
 * A failure of the key store, as one fixed word. The word is what crosses to the Rust side (its error code); nothing the
 * system said about the device or the key is kept, so no message of the platform can reach a log or the page.
 *
 * `cancelled` the person closed the system prompt; `key_gone` the wrapping key does not exist any more (the screen lock was
 * removed, which makes the system delete it) or can no longer be used; `no_screen_lock` an authenticated key was asked for on a
 * device without a screen lock; `invalid` the bytes were not made by this device's key or were changed; `failed` anything else.
 */
class KeystoreFailure(val word: String) : Exception(word)

/**
 * The one wrapping key of the app and the calls that use it (the secret store of the Rust side wraps its data key with it).
 *
 * The key is an AES-256 key that lives in the Android Keystore and never leaves it: in the secure chip (StrongBox) when the
 * device has one, otherwise in the trusted environment. A key is added, never replaced in place: `generate` makes a new one
 * and makes it the one `wrap` uses, while the older ones stay usable by `unwrap` until `retire`. That lets a switch of
 * protection write the new file before the old key is removed. The keys are told apart by a number in the alias
 * (`holzwave-vault-key-<number>`), and a wrapped value starts with the number of the key that made it, then the 12 byte IV,
 * then the ciphertext and its 16 byte tag. The newest key (the highest number) is the current one.
 *
 * Two kinds of key: `auth` needs the screen lock or a biometric for every single use (no time window), through the framework
 * prompt with the cipher as its crypto object; `plain` needs nothing, for a device that has no screen lock. No library is
 * used (no androidx.biometric), and no cryptography is written here: the platform does it.
 */
internal class KeystoreBridge(private val app: Context, private val activityOf: () -> Activity?) {
  private val lock = Any()

  /** The prompt that is on screen, so it can be closed when the activity goes. */
  @Volatile private var openPrompt: CancellationSignal? = null

  private fun store(): KeyStore = KeyStore.getInstance(PROVIDER).apply { load(null) }

  private fun keyguard(): KeyguardManager = app.getSystemService(Context.KEYGUARD_SERVICE) as KeyguardManager

  fun hasScreenLock(): Boolean = keyguard().isDeviceSecure

  private fun alias(number: Int) = "$ALIAS_PREFIX$number"

  /** The numbers of the keys that exist, ascending. */
  private fun numbers(ks: KeyStore): List<Int> =
    ks.aliases().toList().mapNotNull { name ->
      if (name.startsWith(ALIAS_PREFIX)) name.substring(ALIAS_PREFIX.length).toIntOrNull() else null
    }.filter { it > 0 }.sorted()

  private fun keyOf(ks: KeyStore, number: Int): SecretKey? =
    try {
      ks.getKey(alias(number), null) as? SecretKey
    } catch (e: UnrecoverableKeyException) {
      throw KeystoreFailure("key_gone")
    } catch (e: KeyPermanentlyInvalidatedException) {
      throw KeystoreFailure("key_gone")
    }

  /** What the key says about itself, read from the Keystore (not assumed). */
  private fun infoOf(key: SecretKey): KeyInfo =
    SecretKeyFactory.getInstance(key.algorithm, PROVIDER).getKeySpec(key, KeyInfo::class.java) as KeyInfo

  /** Whether the current key is held by the secure chip. False when there is no key: this reports what is, not what could be. */
  fun strongBox(): Boolean = synchronized(lock) {
    try {
      val ks = store()
      val number = numbers(ks).lastOrNull() ?: return@synchronized false
      val key = keyOf(ks, number) ?: return@synchronized false
      infoOf(key).securityLevel == KeyProperties.SECURITY_LEVEL_STRONGBOX
    } catch (e: Exception) {
      false
    }
  }

  /** Makes a new key with this protection and makes it the current one. */
  fun generate(auth: Boolean) = synchronized(lock) {
    try {
      if (auth && !hasScreenLock()) throw KeystoreFailure("no_screen_lock")
      val ks = store()
      val number = (numbers(ks).lastOrNull() ?: 0) + 1
      val wantsChip = app.packageManager.hasSystemFeature(PackageManager.FEATURE_STRONGBOX_KEYSTORE)
      try {
        make(alias(number), auth, wantsChip)
      } catch (e: ProviderException) {
        // StrongBoxUnavailableException is one of these; some devices report a chip that fails with a plain ProviderException.
        if (!wantsChip) throw e
        make(alias(number), auth, false)
      }
    } catch (e: KeystoreFailure) {
      throw e
    } catch (e: Exception) {
      throw KeystoreFailure("failed")
    }
  }

  private fun make(alias: String, auth: Boolean, strongBox: Boolean) {
    val spec = KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
      .setKeySize(256)
      .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
      .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
      .setRandomizedEncryptionRequired(true)
      .apply {
        if (auth) {
          // Every use needs the person: no timeout (0), the screen lock or a strong biometric.
          setUserAuthenticationRequired(true)
          setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG or KeyProperties.AUTH_DEVICE_CREDENTIAL)
          // A key that also accepts the device credential is not made stronger by dying when a fingerprint is added: whoever
          // can add one already knows the credential and could simply unlock. Dying would only turn a harmless act (adding
          // a finger) into the loss of every saved secret. So the key survives an enrolment; removing the screen lock still
          // deletes it, which the system does for every key that needs authentication.
          setInvalidatedByBiometricEnrollment(false)
        }
        if (strongBox) setIsStrongBoxBacked(true)
      }
      .build()
    val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, PROVIDER)
    generator.init(spec)
    generator.generateKey()
  }

  /** Removes every key but the current one. */
  fun retire() = synchronized(lock) {
    try {
      val ks = store()
      val all = numbers(ks)
      for (number in all.dropLast(1)) ks.deleteEntry(alias(number))
    } catch (e: Exception) {
      throw KeystoreFailure("failed")
    }
  }

  /** Removes every key of the app. */
  fun delete() = synchronized(lock) {
    try {
      val ks = store()
      for (number in numbers(ks)) ks.deleteEntry(alias(number))
    } catch (e: Exception) {
      throw KeystoreFailure("failed")
    }
  }

  /** Closes the prompt that is on screen, if any (the activity is going away). */
  fun cancelPrompt() {
    openPrompt?.cancel()
  }

  /**
   * Wraps `plain` with the current key. `done` gets the wrapped value or the failure; for an authenticated key it is called
   * after the person has answered the prompt, from the main thread. The bytes given are zeroed when the call is over.
   */
  fun wrap(plain: ByteArray, done: (ByteArray?, KeystoreFailure?) -> Unit) {
    val cipher: Cipher
    val number: Int
    val needsPerson: Boolean
    try {
      synchronized(lock) {
        val ks = store()
        number = numbers(ks).lastOrNull() ?: throw KeystoreFailure("key_gone")
        val key = keyOf(ks, number) ?: throw KeystoreFailure("key_gone")
        needsPerson = infoOf(key).isUserAuthenticationRequired
        cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, key)
      }
    } catch (e: Throwable) {
      plain.fill(0)
      done(null, failureOf(e))
      return
    }
    val finish = { c: Cipher ->
      try {
        val iv = c.iv
        val sealed = c.doFinal(plain)
        val out = ByteBuffer.allocate(4 + iv.size + sealed.size).putInt(number).put(iv).put(sealed).array()
        sealed.fill(0)
        done(out, null)
      } catch (e: Throwable) {
        done(null, failureOf(e))
      } finally {
        plain.fill(0)
      }
    }
    if (!needsPerson) {
      finish(cipher)
      return
    }
    ask(cipher) { c, failure ->
      if (c == null) {
        plain.fill(0)
        done(null, failure)
      } else {
        finish(c)
      }
    }
  }

  /** Unwraps what `wrap` made. Same calling convention as [wrap]; the bytes given are not changed. */
  fun unwrap(wrapped: ByteArray, done: (ByteArray?, KeystoreFailure?) -> Unit) {
    val cipher: Cipher
    val needsPerson: Boolean
    val sealed: ByteArray
    try {
      synchronized(lock) {
        val ks = store()
        val all = numbers(ks)
        if (all.isEmpty()) throw KeystoreFailure("key_gone")
        if (wrapped.size < HEADER + TAG_BYTES) throw KeystoreFailure("invalid")
        val buffer = ByteBuffer.wrap(wrapped)
        val number = buffer.getInt()
        if (!all.contains(number)) throw KeystoreFailure("invalid")
        val iv = ByteArray(IV_BYTES)
        buffer.get(iv)
        sealed = ByteArray(buffer.remaining())
        buffer.get(sealed)
        val key = keyOf(ks, number) ?: throw KeystoreFailure("key_gone")
        needsPerson = infoOf(key).isUserAuthenticationRequired
        cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(TAG_BYTES * 8, iv))
      }
    } catch (e: Throwable) {
      done(null, failureOf(e))
      return
    }
    val finish = { c: Cipher ->
      try {
        done(c.doFinal(sealed), null)
      } catch (e: Throwable) {
        done(null, failureOf(e))
      }
    }
    if (!needsPerson) {
      finish(cipher)
      return
    }
    ask(cipher) { c, failure -> if (c == null) done(null, failure) else finish(c) }
  }

  /**
   * The system prompt for the screen lock or a strong biometric, with the cipher as its crypto object, so the key is usable
   * for this one operation only. `done` gets the authorised cipher or the failure, on the main thread. A wrong finger does not
   * end the prompt; the person cancels it or gets it right.
   */
  private fun ask(cipher: Cipher, done: (Cipher?, KeystoreFailure?) -> Unit) {
    // The prompt belongs to the activity in use now (after a relaunch of the window it is not the one of the first start); with none
    // there is no screen to show it on.
    val host = activityOf()
    if (host == null) {
      done(null, KeystoreFailure("failed"))
      return
    }
    host.runOnUiThread {
      try {
        val signal = CancellationSignal()
        openPrompt = signal
        val prompt = BiometricPrompt.Builder(host)
          .setTitle("Unlock holzWave")
          .setSubtitle("Confirm with your screen lock or biometrics to open the secrets saved in this app.")
          .setAllowedAuthenticators(
            BiometricManager.Authenticators.BIOMETRIC_STRONG or BiometricManager.Authenticators.DEVICE_CREDENTIAL,
          )
          .build()
        prompt.authenticate(
          BiometricPrompt.CryptoObject(cipher),
          signal,
          host.mainExecutor,
          object : BiometricPrompt.AuthenticationCallback() {
            override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
              openPrompt = null
              val authorised = result.cryptoObject?.cipher
              if (authorised == null) done(null, KeystoreFailure("failed")) else done(authorised, null)
            }

            override fun onAuthenticationError(errorCode: Int, errString: CharSequence?) {
              // The text the system gives is not kept: only the word that its code stands for.
              openPrompt = null
              done(null, KeystoreFailure(wordOfPromptError(errorCode)))
            }
          },
        )
      } catch (e: Throwable) {
        openPrompt = null
        done(null, KeystoreFailure("failed"))
      }
    }
  }

  private fun wordOfPromptError(code: Int): String =
    when (code) {
      BiometricPrompt.BIOMETRIC_ERROR_USER_CANCELED,
      BiometricPrompt.BIOMETRIC_ERROR_CANCELED -> "cancelled"
      BiometricPrompt.BIOMETRIC_ERROR_NO_DEVICE_CREDENTIAL -> "no_screen_lock"
      else -> "failed"
    }

  private fun failureOf(e: Throwable): KeystoreFailure =
    when (e) {
      is KeystoreFailure -> e
      is KeyPermanentlyInvalidatedException, is UnrecoverableKeyException -> KeystoreFailure("key_gone")
      is AEADBadTagException -> KeystoreFailure("invalid")
      is GeneralSecurityException -> KeystoreFailure("failed")
      else -> KeystoreFailure("failed")
    }

  companion object {
    private const val PROVIDER = "AndroidKeyStore"
    private const val ALIAS_PREFIX = "holzwave-vault-key-"
    private const val TRANSFORMATION = "AES/GCM/NoPadding"
    private const val IV_BYTES = 12
    private const val TAG_BYTES = 16
    /** The number of the key and the IV, in front of the ciphertext. */
    private const val HEADER = 4 + IV_BYTES
  }
}
