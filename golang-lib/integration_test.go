package aiomsg

import (
	"fmt"
	"testing"
	"time"
)

const short = 3 * time.Second

func recvWithin(t *testing.T, s *Socket, d time.Duration) ([]byte, bool) {
	t.Helper()
	type result struct {
		data []byte
		ok   bool
	}
	ch := make(chan result, 1)
	go func() {
		data, ok := s.Recv()
		ch <- result{data, ok}
	}()
	select {
	case r := <-ch:
		return r.data, r.ok
	case <-time.After(d):
		return nil, false
	}
}

func TestBindSendsToConnectWithBuffering(t *testing.T) {
	server := NewSocket()
	defer server.Close()
	addr, err := server.Bind("127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}

	client := NewSocket()
	defer client.Close()
	if err := client.Connect(addr); err != nil {
		t.Fatal(err)
	}

	// Sent immediately — exercises buffering until the first peer connects.
	if err := server.Send([]byte("hello")); err != nil {
		t.Fatal(err)
	}

	got, ok := recvWithin(t, client, short)
	if !ok || string(got) != "hello" {
		t.Fatalf("got %q ok=%v", got, ok)
	}
}

func TestRoundRobinDistributesOneEach(t *testing.T) {
	server := NewSocket(WithSendMode(RoundRobin))
	defer server.Close()
	addr, _ := server.Bind("127.0.0.1:0")

	c1 := NewSocket()
	c2 := NewSocket()
	defer c1.Close()
	defer c2.Close()
	_ = c1.Connect(addr)
	_ = c2.Connect(addr)
	time.Sleep(300 * time.Millisecond)

	for i := 0; i < 4; i++ {
		_ = server.Send([]byte{byte(i)})
	}

	seen := map[byte]bool{}
	count1, count2 := 0, 0
	for i := 0; i < 2; i++ {
		if m, ok := recvWithin(t, c1, short); ok {
			seen[m[0]] = true
			count1++
		}
		if m, ok := recvWithin(t, c2, short); ok {
			seen[m[0]] = true
			count2++
		}
	}
	if count1 != 2 || count2 != 2 {
		t.Fatalf("expected 2 each, got c1=%d c2=%d", count1, count2)
	}
	if len(seen) != 4 {
		t.Fatalf("expected all 4 messages, saw %v", seen)
	}
}

func TestPublishSendsToEveryPeer(t *testing.T) {
	server := NewSocket(WithSendMode(Publish))
	defer server.Close()
	addr, _ := server.Bind("127.0.0.1:0")

	c1 := NewSocket()
	c2 := NewSocket()
	defer c1.Close()
	defer c2.Close()
	_ = c1.Connect(addr)
	_ = c2.Connect(addr)
	time.Sleep(300 * time.Millisecond)

	_ = server.Send([]byte("news"))

	if m, ok := recvWithin(t, c1, short); !ok || string(m) != "news" {
		t.Fatalf("c1 got %q ok=%v", m, ok)
	}
	if m, ok := recvWithin(t, c2, short); !ok || string(m) != "news" {
		t.Fatalf("c2 got %q ok=%v", m, ok)
	}
}

func TestIdentityRoutingTargetsOnePeer(t *testing.T) {
	server := NewSocket()
	defer server.Close()
	addr, _ := server.Bind("127.0.0.1:0")

	var id1, id2 Identity
	for i := range id1 {
		id1[i] = 1
		id2[i] = 2
	}
	c1 := NewSocket(WithIdentity(id1))
	c2 := NewSocket(WithIdentity(id2))
	defer c1.Close()
	defer c2.Close()
	_ = c1.Connect(addr)
	_ = c2.Connect(addr)
	time.Sleep(300 * time.Millisecond)

	_ = server.SendTo(id1, []byte("for-one"))
	_ = server.SendTo(id2, []byte("for-two"))

	if m, ok := recvWithin(t, c1, short); !ok || string(m) != "for-one" {
		t.Fatalf("c1 got %q ok=%v", m, ok)
	}
	if m, ok := recvWithin(t, c2, short); !ok || string(m) != "for-two" {
		t.Fatalf("c2 got %q ok=%v", m, ok)
	}
}

func TestAtLeastOnceDeliversAndAcks(t *testing.T) {
	server := NewSocket(WithSendMode(RoundRobin), WithDeliveryGuarantee(AtLeastOnce))
	defer server.Close()
	addr, _ := server.Bind("127.0.0.1:0")

	client := NewSocket()
	defer client.Close()
	_ = client.Connect(addr)
	time.Sleep(200 * time.Millisecond)

	_ = server.Send([]byte("reliable"))

	if m, ok := recvWithin(t, client, short); !ok || string(m) != "reliable" {
		t.Fatalf("got %q ok=%v", m, ok)
	}
	// No duplicate well within the 5s resend window.
	if m, ok := recvWithin(t, client, 500*time.Millisecond); ok {
		t.Fatalf("unexpected duplicate %q", m)
	}
}

func TestMessagesChannelYieldsInOrder(t *testing.T) {
	server := NewSocket(WithSendMode(Publish))
	defer server.Close()
	addr, _ := server.Bind("127.0.0.1:0")

	client := NewSocket()
	defer client.Close()
	_ = client.Connect(addr)
	time.Sleep(200 * time.Millisecond)

	for i := 0; i < 5; i++ {
		_ = server.Send([]byte{byte(i)})
	}

	var got []int
	for i := 0; i < 5; i++ {
		select {
		case m := <-client.Messages():
			got = append(got, int(m.Data[0]))
		case <-time.After(short):
			t.Fatalf("timed out after %d messages", len(got))
		}
	}
	want := []int{0, 1, 2, 3, 4}
	if fmt.Sprint(got) != fmt.Sprint(want) {
		t.Fatalf("got %v want %v", got, want)
	}
}

func TestConnectEndReconnectsAfterServerRestart(t *testing.T) {
	server1 := NewSocket(WithSendMode(Publish))
	addr, _ := server1.Bind("127.0.0.1:0")

	client := NewSocket()
	defer client.Close()
	_ = client.Connect(addr)

	_ = server1.Send([]byte("first"))
	if m, ok := recvWithin(t, client, short); !ok || string(m) != "first" {
		t.Fatalf("got %q ok=%v", m, ok)
	}

	_ = server1.Close()

	server2 := NewSocket(WithSendMode(Publish))
	defer server2.Close()
	if _, err := server2.Bind(addr); err != nil {
		t.Fatal(err)
	}

	_ = server2.Send([]byte("second"))
	if m, ok := recvWithin(t, client, 5*time.Second); !ok || string(m) != "second" {
		t.Fatalf("got %q ok=%v", m, ok)
	}
}
