// TLS configuration from the PEM files used by the conformance suite.
//
// The bind side presents a certificate + private key; the connect side trusts a
// self-signed CA and verifies the peer's name against it. .NET's SslStream works
// directly with X509Certificate2, so the PEM files load with no key-store step.
// Verification uses a custom trust anchor (CustomRootTrust) plus the normal
// hostname/IP-SAN check against the target host, so the shared test cert's
// IP:127.0.0.1 SAN is accepted on loopback.
using System.Net.Security;
using System.Security.Cryptography.X509Certificates;

namespace Aiomsg;

internal static class Tls
{
    /// <summary>Load the server certificate chain + private key from PEM files.</summary>
    public static X509Certificate2 LoadServerCertificate(string certPath, string keyPath)
    {
        using var pem = X509Certificate2.CreateFromPemFile(certPath, keyPath);
        // Round-trip through PKCS#12 so SslStream gets a cert with a usable
        // private key on every platform (a known SslStream/ephemeral-key quirk).
        return X509CertificateLoader.LoadPkcs12(pem.Export(X509ContentType.Pkcs12), null);
    }

    /// <summary>Server-side TLS options presenting the given certificate.</summary>
    public static SslServerAuthenticationOptions ServerOptions(X509Certificate2 certificate) =>
        new()
        {
            ServerCertificate = certificate,
            ClientCertificateRequired = false,
            CertificateRevocationCheckMode = X509RevocationMode.NoCheck,
        };

    /// <summary>
    /// Client-side TLS options trusting <paramref name="caPath"/> as the sole
    /// root and verifying the peer against <paramref name="targetHost"/> (a DNS
    /// name or an IP literal matched against the certificate's SANs).
    /// </summary>
    public static SslClientAuthenticationOptions ClientOptions(string caPath, string targetHost)
    {
        var ca = X509Certificate2.CreateFromPem(File.ReadAllText(caPath));
        return new SslClientAuthenticationOptions
        {
            TargetHost = targetHost,
            CertificateRevocationCheckMode = X509RevocationMode.NoCheck,
            RemoteCertificateValidationCallback = (_, certificate, _, errors) =>
            {
                if (certificate is null)
                    return false;
                // Enforce the name match SslStream computed against TargetHost...
                if ((errors & SslPolicyErrors.RemoteCertificateNameMismatch) != 0)
                    return false;
                // ...but trust our own CA as the root rather than the OS store.
                using var chain = new X509Chain();
                chain.ChainPolicy.TrustMode = X509ChainTrustMode.CustomRootTrust;
                chain.ChainPolicy.RevocationMode = X509RevocationMode.NoCheck;
                chain.ChainPolicy.CustomTrustStore.Add(ca);
                return chain.Build(new X509Certificate2(certificate));
            },
        };
    }
}
