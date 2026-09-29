// Command server runs the order service on :8080.
package main

import (
	"log"
	"net/http"

	"example.com/orders/internal/httpapi"
	"example.com/orders/internal/orders"
)

func main() {
	h := httpapi.New(orders.NewService(orders.NewMemoryStore()))
	log.Println("listening on :8080")
	log.Fatal(http.ListenAndServe(":8080", h))
}
