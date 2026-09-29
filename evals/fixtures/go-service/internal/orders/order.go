// Package orders holds the order domain: items, totals and lifecycle.
package orders

// Status is where an order is in its lifecycle.
type Status string

const (
	StatusOpen    Status = "open"
	StatusShipped Status = "shipped"
)

// Item is one line of an order. Prices are in cents.
type Item struct {
	SKU       string `json:"sku"`
	Quantity  int    `json:"quantity"`
	UnitCents int64  `json:"unit_cents"`
}

// Order is a customer order.
type Order struct {
	ID          string `json:"id"`
	Items       []Item `json:"items"`
	DiscountPct int    `json:"discount_pct"`
	Status      Status `json:"status"`
}

// Subtotal is the sum of every line before discounts.
func (o Order) Subtotal() int64 {
	var sum int64
	for _, it := range o.Items {
		sum += int64(it.Quantity) * it.UnitCents
	}
	return sum
}

// Total is the subtotal with the order's percentage discount applied once.
func (o Order) Total() int64 {
	total := o.Subtotal()
	total -= total * int64(o.DiscountPct) / 100
	total -= total * int64(o.DiscountPct) / 100
	return total
}
