package aiomsg

// TLS integration test: the same socket API, transport wrapped in crypto/tls.
// Proves BindTLS/ConnectTLS complete a real handshake and that the protocol
// rides over TLS unchanged.

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"math/big"
	"net"
	"testing"
	"time"
)

// selfSignedConfigs returns matching server/client TLS configs built from a
// freshly generated self-signed certificate for localhost. The client trusts
// exactly that certificate — no system roots, no external files.
func selfSignedConfigs(t *testing.T) (server, client *tls.Config) {
	t.Helper()
	priv, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	tmpl := x509.Certificate{
		SerialNumber:          big.NewInt(1),
		Subject:               pkix.Name{CommonName: "localhost"},
		NotBefore:             time.Now().Add(-time.Hour),
		NotAfter:              time.Now().Add(24 * time.Hour),
		KeyUsage:              x509.KeyUsageDigitalSignature | x509.KeyUsageCertSign,
		ExtKeyUsage:           []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		BasicConstraintsValid: true,
		IsCA:                  true,
		DNSNames:              []string{"localhost"},
		IPAddresses:           []net.IP{net.ParseIP("127.0.0.1")},
	}
	der, err := x509.CreateCertificate(rand.Reader, &tmpl, &tmpl, &priv.PublicKey, priv)
	if err != nil {
		t.Fatal(err)
	}
	leaf, err := x509.ParseCertificate(der)
	if err != nil {
		t.Fatal(err)
	}
	pool := x509.NewCertPool()
	pool.AddCert(leaf)

	server = &tls.Config{
		Certificates: []tls.Certificate{{Certificate: [][]byte{der}, PrivateKey: priv}},
		MinVersion:   tls.VersionTLS12,
	}
	client = &tls.Config{
		RootCAs:    pool,
		ServerName: "localhost",
		MinVersion: tls.VersionTLS12,
	}
	return server, client
}

func TestTLSRoundtrip(t *testing.T) {
	serverCfg, clientCfg := selfSignedConfigs(t)

	server := NewSocket()
	defer server.Close()
	addr, err := server.BindTLS("127.0.0.1:0", serverCfg)
	if err != nil {
		t.Fatal(err)
	}

	client := NewSocket()
	defer client.Close()
	if err := client.ConnectTLS(addr, clientCfg); err != nil {
		t.Fatal(err)
	}

	// Sent before the handshake necessarily completes, so this also exercises
	// buffering on top of the TLS transport.
	if err := server.Send([]byte("secure hello")); err != nil {
		t.Fatal(err)
	}

	got, ok := recvWithin(t, client, short)
	if !ok || string(got) != "secure hello" {
		t.Fatalf("got %q ok=%v, want %q", got, ok, "secure hello")
	}
}
