// Example gate-level netlist (demo_cells library).
module parity_unit (q, en, a1, rst_n, b0, qn0, sel, par);
  input [1:0] q;
  input en, a1, rst_n, b0, qn0, sel;
  output par;
  wire n2, n3, n4, n5;
  XNOR2_X1 u_xn (.A(q[0]), .B(q[1]), .Y(n2));
  AN2B_X1  u_g0 (.AN(n2), .B(en), .Y(n3));
  OAI21_X1 u_o0 (.A1(n3), .A2(a1), .B(rst_n), .Y(n4));
  AOI22_X1 u_o1 (.A1(n4), .A2(b0), .B1(qn0), .B2(sel), .Y(n5));
  INV_X1   u_i0 (.A(n5), .Y(par));
endmodule

module demo_top (clk, rst_n, en, sel, scan_en, scan_in, oe, a, b, sum, carry, par, bus_out);
  input clk, rst_n, en, sel, scan_en, scan_in, oe;
  input [1:0] a, b;
  output [1:0] sum;
  output carry, par, bus_out;
  wire gclk, c0, carry_i, lo, n1, n6, n7, n8, n9;
  wire [1:0] s_raw, d, q;

  ICG_X1   u_icg (.CK(clk), .E(en), .SE(scan_en), .GCK(gclk));
  XOR2_X1  u_x0  (.A(a[0]), .B(b[0]), .Y(s_raw[0]));
  AND2_X1  u_a0  (.A(a[0]), .B(b[0]), .Y(c0));
  FA_X1    u_fa1 (.A(a[1]), .B(b[1]), .CI(c0), .S(s_raw[1]), .CO(carry_i));
  MUX2_X1  u_m0  (.A(s_raw[0]), .B(q[0]), .S(sel), .Y(d[0]));
  MUX2_X1  u_m1  (.A(s_raw[1]), .B(q[1]), .S(sel), .Y(d[1]));
  MAJ3_X1  u_mj  (.A(a[0]), .B(b[1]), .C(scan_in), .Y(n8));
  DFFR_X1  u_r0  (.D(d[0]), .CK(gclk), .RN(rst_n), .Q(q[0]), .QN(n1));
  SDFF_X1  u_r1  (.D(d[1]), .SI(n8), .SE(scan_en), .CK(gclk), .Q(q[1]));
  BUF_X2   u_b0  (.A(q[0]), .Y(sum[0]));
  BUF_X2   u_b1  (.A(q[1]), .Y(sum[1]));
  parity_unit u_par (.q(q), .en(en), .a1(a[1]), .rst_n(rst_n), .b0(b[0]), .qn0(n1), .sel(sel), .par(par));
  TIELO_X1 u_t0  (.Y(lo));
  NOR2_X1  u_n0  (.A(carry_i), .B(lo), .Y(n6));
  INV_X1   u_i1  (.A(n6), .Y(carry));
  NAND2_X1 u_nd  (.A(par), .B(1'b1), .Y(n9));
  LATN_X1  u_l0  (.D(n9), .GN(clk), .Q(n7));
  OR2B_X1  u_ob  (.AN(n7), .B(oe), .Y(n10));
  OR3_X1   u_or  (.A(n10), .B(c0), .C(sel), .Y(n11));
  TBUF_X1  u_tb  (.A(n11), .OE(oe), .Y(bus_out));
endmodule
