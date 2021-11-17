package main

import (
	"bufio"
	"chatgo/goproto"
	"context"
	"flag"
	"fmt"
	"net"
	"net/textproto"
	"strings"
	"time"

	"google.golang.org/grpc"
)

var (
	senderID   string
	receiverID string
)

func init() {
	flag.StringVar(&senderID, "sender-id", "toai", "")
	flag.StringVar(&receiverID, "receiver-id", "toai2", "")
	flag.Parse()

}

func main() {
	conn, err := grpc.Dial("[::1]:10000", grpc.WithInsecure())
	if err != nil {
		panic(err)
	}

	cl := goproto.NewChatServiceClient(conn)
	stream, err := cl.Subscribe(context.Background(), &goproto.SubscribeRequest{
		UserId: receiverID,
	})
	if err != nil {
		panic(err)
	}
	go consumestream(stream, cl)
	lis, err := net.Listen("tcp", ":8080")
	if err != nil {
		panic(err)
	}
	tcpConn, err := lis.Accept()
	if err != nil {
		panic(err)
	}
	reader := bufio.NewReader(tcpConn)
	tp := textproto.NewReader(reader)
	for {
		line, _ := tp.ReadLine()
		parts := strings.Split(line, " ")
		from, to, msg := parts[0], parts[1], parts[2]
		sendMsg(cl, msg, from, to)
	}
}

func sendMsg(conn goproto.ChatServiceClient, line string, sender, receiver string) {
	_, err := conn.SendMessage(context.Background(), &goproto.SendMessageRequest{
		ReceiverId: receiver,
		SenderId:   sender,
		Message:    line,
	})
	if err != nil {
		panic(err)
	}
}

func consumestream(stream goproto.ChatService_SubscribeClient, conn goproto.ChatServiceClient) {
	event, err := stream.Recv()
	if err != nil {
		panic(err)
	}
	pingevent, ok := event.GetEvent().(*goproto.Event_EventPing)
	if !ok {
		panic("not first ping event")
	}
	fmt.Println("got first event")
	session := pingevent.EventPing.SessionId
	if session == "" {
		panic("empty session id")
	}
	go pingEach3secs(session, conn)
	for {
		event, err := stream.Recv()
		if err != nil {
			panic(err)
		}
		msg := event.GetEventNewMessage()
		if msg != nil {
			sender := msg.UserId
			content := msg.Content
			fmt.Printf("%s: %s\n", sender, content)
		}
	}
}
func pingEach3secs(sessionID string, conn goproto.ChatServiceClient) {
	ticker := time.NewTicker(3 * time.Second)
	for {
		<-ticker.C
		_, err := conn.Ping(context.Background(), &goproto.PingRequest{
			SessionId: sessionID,
			UserId:    receiverID,
		})
		if err != nil {
			panic(err)
		}
		fmt.Println("ping")
	}
}
