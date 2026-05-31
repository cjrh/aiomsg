// Builds JSSE SSLContexts from the PEM files used by the conformance suite.
//
// JSSE works in terms of KeyStores rather than PEM, so this parses the PEM cert
// (and PKCS#8 private key, for servers) into an in-memory KeyStore and wires up
// the key/trust managers. Hostname/IP verification is left to the SSLSocket's
// endpoint-identification algorithm ("HTTPS"), which validates IP-address SANs.
package aiomsg;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.GeneralSecurityException;
import java.security.KeyFactory;
import java.security.KeyStore;
import java.security.PrivateKey;
import java.security.cert.Certificate;
import java.security.cert.CertificateFactory;
import java.security.cert.X509Certificate;
import java.security.spec.PKCS8EncodedKeySpec;
import java.util.ArrayList;
import java.util.Base64;
import java.util.List;
import javax.net.ssl.KeyManagerFactory;
import javax.net.ssl.SSLContext;
import javax.net.ssl.TrustManagerFactory;

final class Tls {
    private Tls() {}

    private static final char[] PASSWORD = "aiomsg".toCharArray();

    // Extract the base64 body of the first PEM block of the given type.
    private static byte[] pemBody(String pem, String label) {
        String begin = "-----BEGIN " + label + "-----";
        String end = "-----END " + label + "-----";
        int b = pem.indexOf(begin);
        int e = pem.indexOf(end);
        if (b < 0 || e < 0) throw new IllegalArgumentException("missing PEM block: " + label);
        String body = pem.substring(b + begin.length(), e).replaceAll("\\s", "");
        return Base64.getDecoder().decode(body);
    }

    private static List<X509Certificate> readCerts(String pem) throws GeneralSecurityException {
        CertificateFactory cf = CertificateFactory.getInstance("X.509");
        List<X509Certificate> out = new ArrayList<>();
        int idx = 0;
        String begin = "-----BEGIN CERTIFICATE-----";
        while ((idx = pem.indexOf(begin, idx)) >= 0) {
            int end = pem.indexOf("-----END CERTIFICATE-----", idx);
            String block = pem.substring(idx, end + "-----END CERTIFICATE-----".length());
            byte[] der = pemBody(block, "CERTIFICATE");
            out.add((X509Certificate) cf.generateCertificate(new java.io.ByteArrayInputStream(der)));
            idx = end + 1;
        }
        return out;
    }

    private static PrivateKey readPkcs8Key(String pem) throws GeneralSecurityException {
        byte[] der = pemBody(pem, "PRIVATE KEY");
        PKCS8EncodedKeySpec spec = new PKCS8EncodedKeySpec(der);
        // Try common algorithms; the conformance cert is EC (P-256).
        for (String algo : new String[] {"EC", "RSA", "EdDSA"}) {
            try {
                return KeyFactory.getInstance(algo).generatePrivate(spec);
            } catch (GeneralSecurityException ignored) {
                // try next
            }
        }
        throw new GeneralSecurityException("unsupported private key algorithm");
    }

    /** SSLContext for a TLS server, presenting the given cert chain + key. */
    static SSLContext serverContext(String certPath, String keyPath) throws IOException, GeneralSecurityException {
        List<X509Certificate> chain = readCerts(Files.readString(Path.of(certPath)));
        PrivateKey key = readPkcs8Key(Files.readString(Path.of(keyPath)));
        KeyStore ks = KeyStore.getInstance("PKCS12");
        ks.load(null, null);
        ks.setKeyEntry("aiomsg", key, PASSWORD, chain.toArray(new Certificate[0]));
        KeyManagerFactory kmf = KeyManagerFactory.getInstance(KeyManagerFactory.getDefaultAlgorithm());
        kmf.init(ks, PASSWORD);
        SSLContext ctx = SSLContext.getInstance("TLS");
        ctx.init(kmf.getKeyManagers(), null, null);
        return ctx;
    }

    /** SSLContext for a TLS client, trusting the given CA / self-signed cert. */
    static SSLContext clientContext(String caPath) throws IOException, GeneralSecurityException {
        List<X509Certificate> cas = readCerts(Files.readString(Path.of(caPath)));
        KeyStore ts = KeyStore.getInstance("PKCS12");
        ts.load(null, null);
        for (int i = 0; i < cas.size(); i++) ts.setCertificateEntry("ca" + i, cas.get(i));
        TrustManagerFactory tmf = TrustManagerFactory.getInstance(TrustManagerFactory.getDefaultAlgorithm());
        tmf.init(ts);
        SSLContext ctx = SSLContext.getInstance("TLS");
        ctx.init(null, tmf.getTrustManagers(), null);
        return ctx;
    }
}
