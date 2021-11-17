
gen-go:
	protoc --proto_path=proto \
	--go_out=goproto --go_opt=paths=source_relative \
	--go-grpc_out=goproto --go-grpc_opt=paths=source_relative \
	chatinfra.proto 

