// Self-contained TLS demo: a bind socket and a connect socket talking over
// crypto/tls in one process.
//
//	go run ./examples/tls
//
// It generates a throwaway self-signed certificate at startup so it runs with
// no setup. A real deployment loads a certificate from PEM files instead, e.g.
// via tls.LoadX509KeyPair, and trusts a CA via x509.CertPool.AppendCertsFromPEM.
package main

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"fmt"
	"math/big"
	"net"
	"time"

	aiomsg "github.com/cjrh/aiomsg/golang-lib"
)

func main() {
	serverCfg, clientCfg := selfSignedConfigs()

	server := aiomsg.NewSocket()
	defer server.Close()
	addr, err := server.BindTLS("127.0.0.1:0", serverCfg)
	if err != nil {
		panic(err)
	}
	fmt.Println("server bound (TLS) on", addr)

	client := aiomsg.NewSocket()
	defer client.Close()
	// The TCP target is addr, but the certificate is validated against the name
	// in clientCfg.ServerName ("localhost") — these are independent.
	if err := client.ConnectTLS(addr, clientCfg); err != nil {
		panic(err)
	}

	_ = server.Send([]byte("hello over TLS"))
	select {
	case m := <-client.Messages():
		fmt.Printf("client received: %s\n", m.Data)
	case <-time.After(5 * time.Second):
		fmt.Println("timed out")
	}
}

// selfSignedConfigs returns matching server/client TLS configs trusting only a
// freshly minted self-signed certificate for localhost.
func selfSignedConfigs() (server, client *tls.Config) {
	priv, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		panic(err)
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
		panic(err)
	}
	leaf, err := x509.ParseCertificate(der)
	if err != nil {
		panic(err)
	}
	pool := x509.NewCertPool()
	pool.AddCert(leaf)

	server = &tls.Config{
		Certificates: []tls.Certificate{{Certificate: [][]byte{der}, PrivateKey: priv}},
		MinVersion:   tls.VersionTLS12,
	}
	client = &tls.Config{RootCAs: pool, ServerName: "localhost", MinVersion: tls.VersionTLS12}
	return server, client
}
